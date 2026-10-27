use std::sync::OnceLock;

use utoipa::{
    OpenApi,
    openapi::{
        Ref, RefOr,
        content::ContentBuilder,
        response::{Response, ResponseBuilder},
    },
};
use utoipa_axum::router::OpenApiRouter;

use super::{
    api_routes,
    error::{ArchiveReason, ErrorBody, ErrorCode, ErrorDetails, ErrorEnvelope, ErrorScope, Retry},
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "canterlot hub API",
        version = "1.0.0",
        description = "HTTP и WSS контракт хаба. Личность задаёт прокси перед хабом; изменяющим запросам нужны Origin, X-Canterlot-Client: 1 и Idempotency-Key."
    ),
    components(schemas(
        ErrorCode,
        ErrorScope,
        Retry,
        ErrorEnvelope,
        ErrorBody,
        ErrorDetails,
        ArchiveReason
    ))
)]
struct ApiDoc;

/// Документ собирается из тех же `OpenApiRouter`, из которых строится маршрутизатор.
pub fn document() -> utoipa::openapi::OpenApi {
    let mut base = ApiDoc::openapi();
    // Лицензия в derive по умолчанию пустая и нарушает схему OpenAPI.
    base.info.license = None;
    let mut document = OpenApiRouter::with_openapi(base)
        .merge(api_routes())
        .into_openapi();
    add_default_error(&mut document);
    document
}

/// Любая операция может завершиться ошибкой из реестра кодов, поэтому каждая получает ответ `default`.
fn add_default_error(document: &mut utoipa::openapi::OpenApi) {
    let response: Response = ResponseBuilder::new()
        .description("Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry")
        .content(
            "application/json",
            ContentBuilder::new()
                .schema(Some(Ref::from_schema_name("ErrorEnvelope")))
                .build(),
        )
        .build();
    for item in document.paths.paths.values_mut() {
        for operation in [
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.options,
            &mut item.head,
            &mut item.patch,
            &mut item.trace,
        ]
        .into_iter()
        .flatten()
        {
            operation
                .responses
                .responses
                .entry("default".to_owned())
                .or_insert_with(|| RefOr::T(response.clone()));
        }
    }
}

/// Текст `docs/api/openapi.json`: красивый JSON с завершающим переводом строки.
pub fn render() -> String {
    let mut text = document()
        .to_pretty_json()
        .expect("the OpenAPI document serialises");
    text.push('\n');
    text
}

pub(crate) fn rendered() -> &'static str {
    static RENDERED: OnceLock<String> = OnceLock::new();
    RENDERED.get_or_init(render)
}
