//! Место по IP на самом хабе (спецификация §8): база DB-IP «IP to City Lite» (MMDB,
//! CC BY 4.0) через отображение в память. Адреса никуда не уходят.

use std::{
    collections::BTreeMap,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use maxminddb::{Mmap, Reader};
use serde::{Deserialize, Serialize};

/// Место сеанса, как оно хранится в `created_location` / `last_location`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub city: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    /// Частная, петлевая, link-local или CGNAT сеть: «локальная сеть».
    #[serde(default)]
    pub local: bool,
}

impl Location {
    fn local() -> Self {
        Self {
            local: true,
            ..Default::default()
        }
    }

    /// Короткое место для пушей: город, иначе регион, иначе страна.
    pub fn place(&self) -> String {
        if self.local {
            return "локальная сеть".into();
        }
        self.city
            .as_deref()
            .or(self.region.as_deref())
            .or(self.country.as_deref())
            .unwrap_or_default()
            .to_owned()
    }
}

/// Частные, петлевые, link-local и CGNAT адреса в базе не ищутся.
pub fn is_local(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

#[derive(Deserialize)]
struct Names {
    #[serde(default)]
    names: BTreeMap<String, String>,
}

impl Names {
    fn pick(&self) -> Option<String> {
        self.names
            .get("ru")
            .or_else(|| self.names.get("en"))
            .filter(|name| !name.is_empty())
            .cloned()
    }
}

#[derive(Deserialize)]
struct Country {
    #[serde(default)]
    names: BTreeMap<String, String>,
    iso_code: Option<String>,
}

#[derive(Deserialize)]
struct CityRecord {
    city: Option<Names>,
    #[serde(default)]
    subdivisions: Vec<Names>,
    country: Option<Country>,
}

struct Opened {
    reader: Option<Arc<Reader<Mmap>>>,
    modified: Option<SystemTime>,
    checked: Option<Instant>,
}

/// База мест: открыта через `mmap`, перечитывается, когда у файла меняется mtime
/// (проверка не чаще `interval`). Нет файла или он испорчен — места нет.
pub struct Geo {
    path: PathBuf,
    interval: Duration,
    opened: Mutex<Opened>,
}

impl Geo {
    pub fn open(path: impl AsRef<Path>) -> Self {
        Self::with_check_interval(path, Duration::from_secs(60))
    }

    pub fn with_check_interval(path: impl AsRef<Path>, interval: Duration) -> Self {
        Self {
            path: path.as_ref().to_owned(),
            interval,
            opened: Mutex::new(Opened {
                reader: None,
                modified: None,
                checked: None,
            }),
        }
    }

    pub fn lookup(&self, ip: IpAddr) -> Option<Location> {
        let ip = ip.to_canonical();
        if is_local(ip) {
            return Some(Location::local());
        }
        let reader = self.reader()?;
        let record: CityRecord = reader.lookup(ip).ok()?.decode().ok()??;
        let country = record.country;
        let location = Location {
            city: record.city.as_ref().and_then(Names::pick),
            region: record.subdivisions.first().and_then(Names::pick),
            country: country.as_ref().and_then(|country| {
                Names {
                    names: country.names.clone(),
                }
                .pick()
            }),
            country_code: country.and_then(|country| country.iso_code),
            local: false,
        };
        (location != Location::default()).then_some(location)
    }

    fn reader(&self) -> Option<Arc<Reader<Mmap>>> {
        let mut opened = self.opened.lock().unwrap_or_else(|e| e.into_inner());
        let due = opened
            .checked
            .is_none_or(|checked| checked.elapsed() >= self.interval);
        if due {
            opened.checked = Some(Instant::now());
            let modified = std::fs::metadata(&self.path)
                .and_then(|meta| meta.modified())
                .ok();
            if modified != opened.modified {
                opened.modified = modified;
                opened.reader = modified.and_then(|_| open_reader(&self.path));
                if modified.is_some() {
                    tracing::info!(
                        kind = "geoip_opened",
                        ready = opened.reader.is_some(),
                        "geo base (re)opened"
                    );
                }
            }
        }
        opened.reader.clone()
    }
}

/// Открывает файл через `mmap`. Обновление кладёт новый файл переименованием, поэтому
/// отображённый файл никто не меняет и не укорачивает.
#[allow(unsafe_code)]
pub fn open_reader(path: &Path) -> Option<Arc<Reader<Mmap>>> {
    // SAFETY: файл не изменяется на месте: `geoip update` пишет новый файл рядом и
    // переименовывает его, старый inode остаётся целым, пока отображение живо.
    match unsafe { Reader::open_mmap(path) } {
        Ok(reader) => Some(Arc::new(reader)),
        Err(error) => {
            tracing::warn!(kind = "geoip_unreadable", error = %error, "geo base cannot be opened");
            None
        }
    }
}

/// Откуда `canterlot-hub geoip update` берёт базу.
pub const DBIP_BASE_URL: &str = "https://download.db-ip.com/free";
/// Адрес для проверки скачанной базы: поиск должен пройти без ошибки.
const PROBE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(1, 1, 1, 1));

fn previous_month(date: chrono::NaiveDate) -> chrono::NaiveDate {
    use chrono::Datelike;
    let first = date.with_day(1).unwrap_or(date);
    first.pred_opt().unwrap_or(first)
}

/// Скачивает базу DB-IP City Lite за месяц `today`, а если её ещё нет — за прошлый,
/// распаковывает во временный файл рядом с `target`, проверяет открытием и одним поиском
/// и переименовывает поверх `target`. При любой ошибке прежний файл остаётся.
/// Запрос несёт только имя файла: ни адресов, ни данных владельца.
pub async fn update(
    target: &Path,
    base_url: &str,
    today: chrono::NaiveDate,
) -> Result<String, String> {
    use chrono::Datelike;
    let dir = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|_| "geoip_dir_error")?;
    let client = reqwest::Client::builder()
        .user_agent("canterlot-hub")
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|_| "geoip_client_error")?;
    let base = base_url.trim_end_matches('/');
    for month in [today, previous_month(today)] {
        let name = format!("dbip-city-lite-{}-{:02}", month.year(), month.month());
        let response = client
            .get(format!("{base}/{name}.mmdb.gz"))
            .send()
            .await
            .map_err(|_| "geoip_download_failed")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            continue;
        }
        if !response.status().is_success() {
            return Err(format!(
                "geoip_download_failed: HTTP {}",
                response.status().as_u16()
            ));
        }
        let temp = tempfile::Builder::new()
            .prefix(".geoip-")
            .suffix(".tmp")
            .tempfile_in(dir)
            .map_err(|_| "geoip_temp_error")?;
        unpack(response, temp.as_file()).await?;
        let path = temp.path().to_owned();
        tokio::task::spawn_blocking(move || validate(&path))
            .await
            .map_err(|_| "geoip_invalid")??;
        temp.persist(target).map_err(|_| "geoip_rename_failed")?;
        return Ok(name);
    }
    Err("geoip_not_published".into())
}

async fn unpack(mut response: reqwest::Response, file: &std::fs::File) -> Result<(), String> {
    use std::io::Write;
    let mut decoder = flate2::write::GzDecoder::new(file);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "geoip_download_failed")?
    {
        decoder.write_all(&chunk).map_err(|_| "geoip_not_gzip")?;
    }
    decoder
        .try_finish()
        .and_then(|()| decoder.get_ref().sync_all())
        .map_err(|_| "geoip_not_gzip".into())
}

fn validate(path: &Path) -> Result<(), String> {
    let reader = open_reader(path).ok_or("geoip_invalid")?;
    let probe = reader.lookup(PROBE).map_err(|_| "geoip_invalid")?;
    if probe.has_data() {
        probe.decode::<CityRecord>().map_err(|_| "geoip_invalid")?;
    }
    Ok(())
}
