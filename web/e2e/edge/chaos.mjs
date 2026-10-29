export class ChaosController {
  constructor() {
    this.blackholeActive = false;
    this.wsBlackholeActive = false;
    this.latencyMs = 0;
    this.rewriteSubscribeDelta = null;
    this.rewriteScopePrefix = 'conversation:';
    this.dropResponsePattern = null;
    this.dropResponseMethod = 'POST';
    this.httpInFlight = new Set();
    this.cutDuring = null;
    this.httpSockets = new Set();
    this.recording = false;
    this.frames = [];
    this.activeWsSockets = new Set();
  }

  registerWsSocket(clientSocket, hubSocket) {
    const pair = { clientSocket, hubSocket };
    this.activeWsSockets.add(pair);
    const cleanup = () => this.activeWsSockets.delete(pair);
    clientSocket.on('close', cleanup);
    clientSocket.on('error', cleanup);
    hubSocket.on('close', cleanup);
    hubSocket.on('error', cleanup);
  }

  get wsDropped() {
    return this.blackholeActive || this.wsBlackholeActive;
  }

  /** Взведённый обрыв посреди запроса: после `skip` подходящих запросов рвёт следующий, когда через кромку прошло `afterBytes` тела. */
  takeCutDuring(method, pathname) {
    const armed = this.cutDuring;
    if (!armed || armed.method !== method || !armed.regex.test(pathname)) return null;
    if (armed.skip > 0) {
      armed.skip -= 1;
      return null;
    }
    this.cutDuring = null;
    return armed.afterBytes;
  }

  trackHttp(entry) {
    this.httpInFlight.add(entry);
    const done = () => this.httpInFlight.delete(entry);
    entry.response.on('close', done);
    return done;
  }

  apply(command) {
    switch (command.action) {
      case 'cut': {
        const killedHttp = this.httpInFlight.size;
        for (const entry of [...this.httpInFlight]) {
          try { entry.proxyReq?.destroy(); } catch {}
          try { entry.response.destroy(); } catch {}
        }
        this.httpInFlight.clear();
        for (const pair of [...this.activeWsSockets]) {
          try { pair.clientSocket.destroy(); } catch {}
          try { pair.hubSocket.destroy(); } catch {}
        }
        this.activeWsSockets.clear();
        return { ok: true, action: 'cut', killedHttp };
      }
      case 'blackhole': {
        if (command.ws) this.wsBlackholeActive = Boolean(command.on);
        else this.blackholeActive = Boolean(command.on);
        return { ok: true, blackhole: this.blackholeActive, wsBlackhole: this.wsBlackholeActive };
      }
      case 'latency': {
        this.latencyMs = Math.max(0, Number(command.ms) || 0);
        return { ok: true, latencyMs: this.latencyMs };
      }
      case 'close4401': {
        for (const pair of [...this.activeWsSockets]) {
          try {
            // Send close frame with code 4401 and reason "reauth"
            const payload = Buffer.alloc(2 + 6);
            payload.writeUInt16BE(4401, 0);
            payload.write('reauth', 2, 'utf8');
            const frame = encodeWsFrame(payload, 8, false);
            pair.clientSocket.write(frame);
            pair.clientSocket.end();
            pair.hubSocket.destroy();
          } catch {}
        }
        this.activeWsSockets.clear();
        return { ok: true, action: 'close4401' };
      }
      case 'rewrite_subscribe_after': {
        this.rewriteSubscribeDelta = Number(command.delta);
        this.rewriteScopePrefix = typeof command.scopePrefix === 'string' ? command.scopePrefix : 'conversation:';
        return { ok: true, delta: this.rewriteSubscribeDelta };
      }
      case 'drop_response_once': {
        // Браузер повторяет запрос сам, если пустой ответ пришёл на давно открытом соединении: закрываем простаивающие, чтобы запрос ушёл на свежем.
        const busy = new Set([...this.httpInFlight].map((entry) => entry.response.socket));
        const ws = new Set([...this.activeWsSockets].map((pair) => pair.clientSocket));
        for (const socket of [...this.httpSockets]) {
          if (!busy.has(socket) && !ws.has(socket)) socket.destroy();
        }
        this.dropResponsePattern = new RegExp(command.pathRegex);
        this.dropResponseMethod = String(command.method ?? 'POST').toUpperCase();
        return { ok: true, pattern: command.pathRegex };
      }
      case 'cut_during': {
        this.cutDuring = {
          regex: new RegExp(command.pathRegex),
          method: String(command.method ?? 'POST').toUpperCase(),
          afterBytes: Number(command.afterBytes ?? 1),
          skip: Number(command.skip ?? 0)
        };
        return { ok: true, armed: true };
      }
      case 'record': {
        this.recording = Boolean(command.on);
        if (this.recording) this.frames = [];
        return { ok: true, recording: this.recording };
      }
      default:
        return { error: 'unknown_action' };
    }
  }

  logFrame(direction, parsedOrRaw, delivered = true) {
    if (!this.recording) return;
    this.frames.push({
      ...(typeof parsedOrRaw === 'object' && parsedOrRaw !== null ? parsedOrRaw : {}),
      direction,
      timestamp: Date.now(),
      delivered
    });
  }

  getFrames() {
    return [...this.frames];
  }
}

export function decodeWsFrame(buf) {
  if (buf.length < 2) return null;
  const b0 = buf[0];
  const b1 = buf[1];
  const fin = (b0 & 0x80) !== 0;
  const opcode = b0 & 0x0f;
  const masked = (b1 & 0x80) !== 0;
  let len = b1 & 0x7f;
  let offset = 2;
  if (len === 126) {
    if (buf.length < offset + 2) return null;
    len = buf.readUInt16BE(offset);
    offset += 2;
  } else if (len === 127) {
    if (buf.length < offset + 8) return null;
    len = Number(buf.readBigUInt64BE(offset));
    offset += 8;
  }
  let maskKey = null;
  if (masked) {
    if (buf.length < offset + 4) return null;
    maskKey = buf.subarray(offset, offset + 4);
    offset += 4;
  }
  if (buf.length < offset + len) return null;
  const payload = Buffer.from(buf.subarray(offset, offset + len));
  if (masked) {
    for (let i = 0; i < len; i++) {
      payload[i] ^= maskKey[i % 4];
    }
  }
  return { fin, opcode, payload, totalLength: offset + len };
}

export function encodeWsFrame(payload, opcode = 1, mask = false, fin = true) {
  const len = payload.length;
  let headerLen = 2;
  if (len >= 126 && len <= 65535) headerLen += 2;
  else if (len > 65535) headerLen += 8;
  if (mask) headerLen += 4;
  const buf = Buffer.alloc(headerLen + len);
  buf[0] = (fin ? 0x80 : 0) | (opcode & 0x0f);
  let offset = 2;
  if (len < 126) {
    buf[1] = (mask ? 0x80 : 0) | len;
  } else if (len <= 65535) {
    buf[1] = (mask ? 0x80 : 0) | 126;
    buf.writeUInt16BE(len, 2);
    offset += 2;
  } else {
    buf[1] = (mask ? 0x80 : 0) | 127;
    buf.writeBigUInt64BE(BigInt(len), 2);
    offset += 8;
  }
  if (mask) {
    const maskKey = Buffer.from([0x1a, 0x2b, 0x3c, 0x4d]);
    maskKey.copy(buf, offset);
    offset += 4;
    for (let i = 0; i < len; i++) {
      buf[offset + i] = payload[i] ^ maskKey[i % 4];
    }
  } else {
    payload.copy(buf, offset);
  }
  return buf;
}
