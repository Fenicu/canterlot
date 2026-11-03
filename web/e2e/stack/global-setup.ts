import { execSync, spawn, type ChildProcess } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, writeFileSync, chmodSync, readFileSync, appendFileSync as fsAppend } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createServer } from 'node:net';
import crypto from 'node:crypto';
import { stopStack } from './global-teardown';

export interface Stack {
  baseUrl: string;
  hubUrl: string;
  control: string /* chaos control port */;
  dataDir: string;
  workRoot: string;
  machineName: 'e2e';
  workerDataDir?: string;
  proxySecret: string;
  psql(sql: string): Promise<string>;
}

const STACK_INFO_FILE = join(tmpdir(), 'canterlot-stack-info.json');

async function getFreePort(): Promise<number> {
  return new Promise((res, rej) => {
    const s = createServer();
    s.listen(0, '127.0.0.1', () => {
      const addr = s.address();
      const port = typeof addr === 'object' && addr ? addr.port : 0;
      s.close(err => (err ? rej(err) : res(port)));
    });
  });
}

export function getStack(): Stack {
  if (!existsSync(STACK_INFO_FILE)) {
    throw new Error(`Stack info file not found at ${STACK_INFO_FILE}. Ensure global-setup ran.`);
  }
  const info = JSON.parse(readFileSync(STACK_INFO_FILE, 'utf8'));
  return {
    baseUrl: info.baseUrl,
    hubUrl: info.hubUrl,
    control: String(info.control),
    dataDir: info.dataDir,
    workRoot: info.workRoot,
    machineName: 'e2e',
    workerDataDir: info.workerDataDir,
    proxySecret: info.proxySecret ?? '',
    psql(sql: string): Promise<string> {
      return new Promise((resolvePsql, rejectPsql) => {
        const proc = spawn('psql', ['-h', '127.0.0.1', '-p', String(info.pgPort), '-U', 'canterlot', '-d', info.dbName, '-t', '-A', '-c', sql], {
          env: { ...process.env, PGPASSWORD: 'canterlot' }
        });
        let out = '';
        let err = '';
        proc.stdout.on('data', d => out += d);
        proc.stderr.on('data', d => err += d);
        proc.on('close', code => {
          if (code === 0) resolvePsql(out.trim());
          else rejectPsql(new Error(`psql exit ${code}: ${err}`));
        });
      });
    }
  };
}

export default async function globalSetup() {
  await stopStack();
  const rootRepo = resolve(process.cwd(), '..');
  const pgPort = Number(process.env.CANTERLOT_TEST_PG_PORT || 55440);

  // 1. Ensure test database is running
  try {
    execSync(`CANTERLOT_TEST_PG_PORT=${pgPort} bash ${join(rootRepo, 'scripts/test-db.sh')} up`, {
      stdio: 'pipe',
      timeout: 30000
    });
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    throw new Error(`Failed to ensure test postgres on port ${pgPort}: ${msg}`);
  }

  // 2. Create fresh database for stack tests
  const dbName = `canterlot_stack_${crypto.randomBytes(4).toString('hex')}`;
  execSync(`PGPASSWORD=canterlot psql -h 127.0.0.1 -p ${pgPort} -U canterlot -d canterlot -c "CREATE DATABASE ${dbName};"`, {
    stdio: 'pipe'
  });

  // 3. Prepare temporary directories
  const stackDir = mkdtempSync(join(tmpdir(), 'canterlot-stack-'));
  const dataDir = join(stackDir, 'data');
  const blobDir = join(dataDir, 'blobs');
  const workRoot = join(stackDir, 'work');
  const certsDir = join(stackDir, 'certs');
  const workerDataDir = join(stackDir, 'worker-data');
  mkdirSync(dataDir, { recursive: true });
  mkdirSync(blobDir, { recursive: true });
  mkdirSync(workRoot, { recursive: true });
  mkdirSync(certsDir, { recursive: true });
  mkdirSync(workerDataDir, { recursive: true });
  chmodSync(stackDir, 0o700);
  chmodSync(dataDir, 0o700);
  chmodSync(blobDir, 0o700);
  chmodSync(workRoot, 0o700);
  chmodSync(certsDir, 0o700);
  chmodSync(workerDataDir, 0o700);

  // 4. Generate throw-away CA and server TLS certificates
  const caKey = join(certsDir, 'ca.key');
  const caCrt = join(certsDir, 'ca.crt');
  const serverKey = join(certsDir, 'server.key');
  const serverCsr = join(certsDir, 'server.csr');
  const serverCrt = join(certsDir, 'server.crt');
  const extFile = join(certsDir, 'extfile.cnf');

  writeFileSync(extFile, 'subjectAltName=DNS:localhost,IP:127.0.0.1\n', 'utf8');
  execSync(`openssl req -x509 -newkey rsa:2048 -nodes -keyout ${caKey} -out ${caCrt} -days 1 -subj "/CN=Canterlot Test CA" 2>/dev/null`);
  execSync(`openssl req -newkey rsa:2048 -nodes -keyout ${serverKey} -out ${serverCsr} -subj "/CN=localhost" 2>/dev/null`);
  execSync(`openssl x509 -req -in ${serverCsr} -CA ${caCrt} -CAkey ${caKey} -CAcreateserial -out ${serverCrt} -days 1 -extfile ${extFile} 2>/dev/null`);

  // 5. Proxy secret (>= 32 bytes) & database URL file
  const proxySecret = crypto.randomBytes(32).toString('hex');
  const proxySecretFile = join(stackDir, 'proxy_secret');
  writeFileSync(proxySecretFile, proxySecret, { mode: 0o600 });

  const dbUrl = `postgres://canterlot:canterlot@127.0.0.1:${pgPort}/${dbName}`;
  const dbUrlFile = join(stackDir, 'db_url');
  writeFileSync(dbUrlFile, dbUrl, { mode: 0o600 });

  const manifestFile = join(stackDir, 'installation.toml');

  // Allocate ports
  const hubGrpcPort = await getFreePort();
  const hubHttpPort = await getFreePort();
  const edgePort = await getFreePort();
  const chaosPort = await getFreePort();

  // 6. Write hub.toml
  const hubToml = join(stackDir, 'hub.toml');
  const hubConfigContent = `
database_url_file = "${dbUrlFile}"
grpc_listen = "127.0.0.1:${hubGrpcPort}"
tls_cert = "${serverCrt}"
tls_key = "${serverKey}"
manifest = "${manifestFile}"

[http]
listen = "127.0.0.1:${hubHttpPort}"
trusted_proxies = ["127.0.0.1/32"]
owner_login = "owner"
proxy_secret_file = "${proxySecretFile}"
logout_url = "https://127.0.0.1/logout"
allowed_origins = ["http://127.0.0.1:${edgePort}"]

[http.ws]
max_connection_age = "20s"
poll_interval = "100ms"
delta_window = "20ms"

[attachments]
blob_dir = "${blobDir}"

[limits]
reset_grace = "5s"
verify_backoff = ["1s", "2s"]
`;
  writeFileSync(hubToml, hubConfigContent, 'utf8');

  // 7. Find binaries (built with synthetic-adapter)
  const hubBin = join(rootRepo, 'target/debug/canterlot-hub');
  const workerBin = join(rootRepo, 'target/debug/canterlot-worker');
  if (!existsSync(hubBin) || !existsSync(workerBin)) {
    execSync(`cargo build -p canterlot-hub -p canterlot-worker --features canterlot-worker/synthetic-adapter`, {
      cwd: rootRepo,
      env: { ...process.env, PATH: `${process.env.HOME}/.cargo/bin:${process.env.PATH}` },
      stdio: 'inherit'
    });
  }

  // 8. Run hub init
  execSync(`${hubBin} init --config ${hubToml}`, { stdio: 'pipe' });

  // 9. Run machine add --name e2e
  const machineOutput = execSync(`${hubBin} machine add --config ${hubToml} --name e2e`, { encoding: 'utf8' });
  const machineMatch = machineOutput.match(/machine_id=([^\n]+)\ninstallation_id=([^\n]+)\nrestore_generation=([^\n]+)\ntoken=([^\n]+)/);
  if (!machineMatch) {
    throw new Error(`Failed to parse machine add output: ${machineOutput}`);
  }
  const [, machineId, installationId, restoreGeneration, token] = machineMatch;

  const tokenFile = join(stackDir, 'worker-token');
  writeFileSync(tokenFile, token, { mode: 0o600 });
  chmodSync(tokenFile, 0o600);

  // 10. Write worker.toml
  const workerToml = join(stackDir, 'worker.toml');
  const workerConfigContent = `
hub_url = "https://127.0.0.1:${hubGrpcPort}"
ca_cert = "${caCrt}"
token_file = "${tokenFile}"
machine_id = "${machineId}"
installation_id = "${installationId}"
restore_generation = "${restoreGeneration}"
data_dir = "${workerDataDir}"

[machine]
metrics_interval = "2s"

[execution]
allowed_workdir_roots = ["${workRoot}"]
`;
  writeFileSync(workerToml, workerConfigContent, 'utf8');

  // 11. Spawn Hub process
  const logDir = resolve(process.cwd(), 'e2e/.stack');
  mkdirSync(logDir, { recursive: true });
  const hubLogFile = join(logDir, 'hub.log');
  const workerLogFile = join(logDir, 'worker.log');
  const edgeLogFile = join(logDir, 'edge.log');
  writeFileSync(hubLogFile, '', 'utf8');
  writeFileSync(workerLogFile, '', 'utf8');
  writeFileSync(edgeLogFile, '', 'utf8');

  let hubLogs = '';
  const hubProcess: ChildProcess = spawn(hubBin, ['serve', '--config', hubToml], {
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: false
  });
  hubProcess.stdout?.on('data', d => { hubLogs += d; fsAppend(hubLogFile, d); });
  hubProcess.stderr?.on('data', d => { hubLogs += d; fsAppend(hubLogFile, d); });

  // 12. Spawn Worker process
  let workerLogs = '';
  // Свой каталог сокетов: воркер стенда не видит и не убирает сокеты настоящего воркера.
  const workerRuntime = join(stackDir, 'runtime');
  mkdirSync(workerRuntime, { recursive: true, mode: 0o700 });
  const workerProcess: ChildProcess = spawn(workerBin, ['run', '--config', workerToml], {
    env: { ...process.env, XDG_RUNTIME_DIR: workerRuntime },
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: false
  });
  workerProcess.stdout?.on('data', d => { workerLogs += d; fsAppend(workerLogFile, d); });
  workerProcess.stderr?.on('data', d => { workerLogs += d; fsAppend(workerLogFile, d); });

  // 13. Spawn Edge Server
  let edgeLogs = '';
  const edgeScript = resolve(process.cwd(), 'e2e/edge/serve.mjs');
  const edgeProcess: ChildProcess = spawn('node', [edgeScript, `--stack=${stackDir}`], {
    env: {
      ...process.env,
      PORT: String(edgePort),
      HUB_URL: `http://127.0.0.1:${hubHttpPort}`,
      PROXY_SECRET: proxySecret,
      PROXY_SECRET_FILE: proxySecretFile,
      CONTROL_PORT: String(chaosPort),
      WEB_BUILD: resolve(process.cwd(), 'build')
    },
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: false
  });
  edgeProcess.stdout?.on('data', d => { edgeLogs += d; fsAppend(edgeLogFile, d); });
  edgeProcess.stderr?.on('data', d => { edgeLogs += d; fsAppend(edgeLogFile, d); });

  // 14. Wait for Hub /readyz or /healthz and machine available through edge
  const deadline = Date.now() + 30000;
  let ready = false;
  let lastErr = '';
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${edgePort}/readyz`);
      if (res.ok) {
        // Also check if machine is available
        const machinesRes = await fetch(`http://127.0.0.1:${edgePort}/api/v1/machines`, {
          headers: { 'Remote-User': 'owner' }
        });
        if (machinesRes.ok) {
          const list = (await machinesRes.json()) as Array<{ name?: string; availability?: string; state?: string }>;
          const e2e = list.find(m => m.name === 'e2e');
          if (e2e && (e2e.availability === 'available' || e2e.state === 'available')) {
            ready = true;
            break;
          } else {
            lastErr = `Machine not available: ${JSON.stringify(list)}`;
          }
        } else {
          lastErr = `GET /machines status ${machinesRes.status}`;
        }
      } else {
        lastErr = `GET /readyz status ${res.status}`;
      }
    } catch (e: unknown) {
      lastErr = e instanceof Error ? e.message : String(e);
    }
    await new Promise(r => setTimeout(r, 200));
  }

  if (!ready) {
    process.stderr.write(`[global-setup] HUB LOGS:\n${hubLogs}\n`);
    process.stderr.write(`[global-setup] WORKER LOGS:\n${workerLogs}\n`);
    process.stderr.write(`[global-setup] EDGE LOGS:\n${edgeLogs}\n`);
    process.stderr.write(`[global-setup] LAST ERROR: ${lastErr}\n`);
    hubProcess.kill('SIGKILL');
    workerProcess.kill('SIGKILL');
    edgeProcess.kill('SIGKILL');
    throw new Error(`Timed out waiting for Hub and Worker to become ready through edge proxy. Last: ${lastErr}`);
  }

  // 15. Save Stack state
  const stackInfo = {
    baseUrl: `http://127.0.0.1:${edgePort}`,
    hubUrl: `http://127.0.0.1:${hubHttpPort}`,
    control: String(chaosPort),
    dataDir,
    workerDataDir,
    workRoot,
    machineName: 'e2e',
    pgPort,
    dbName,
    proxySecret,
    pids: {
      hub: hubProcess.pid,
      worker: workerProcess.pid,
      edge: edgeProcess.pid
    },
    stackDir
  };
  writeFileSync(STACK_INFO_FILE, JSON.stringify(stackInfo, null, 2), 'utf8');
}
