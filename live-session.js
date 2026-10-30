(function (root, factory) {
  if (typeof module === 'object' && module.exports) module.exports = factory();
  else root.RelaySession = factory();
})(typeof globalThis === 'object' ? globalThis : this, function () {
  'use strict';
  class RequestError extends Error {
    constructor(message, category, status = null, definitiveRejected = false) {
      super(message);
      this.name = category === 'aborted' ? 'AbortError' : 'RequestError';
      this.category = category; this.status = status; this.definitiveRejected = definitiveRejected;
    }
  }
  function createApi({ getToken, fetchImpl = globalThis.fetch }) {
    return async function api(path, value, raw = false, options = {}) {
      const external = options.signal, controller = new AbortController();
      if (external?.aborted) throw new RequestError('请求已中止；后台提交结果仍需核实。', 'aborted');
      const route = path.split('?')[0], mutation = value !== undefined;
      const deadline = route === '/api/setup/recheck' ? 120000 :
        route === '/api/session' || !mutation && route !== '/api/document' ? 10000 : 30000;
      let timer, onAbort;
      const cancelled = new Promise((resolve, reject) => {
        const cancel = category => {
          reject(new RequestError(category === 'timeout' ? '请求超时；提交结果可能尚未确认。' : '请求已中止；后台提交结果仍需核实。', category));
          controller.abort();
        };
        timer = setTimeout(() => cancel('timeout'), deadline);
        if (external) { onAbort = () => cancel('aborted'); external.addEventListener('abort', onAbort, { once: true }); }
      });
      const request = (async () => {
        let response;
        try {
          const token = getToken();
          response = await fetchImpl(path, { method: mutation ? 'POST' : 'GET', cache: 'no-store', credentials: 'omit',
            headers: { ...(mutation ? { 'Content-Type': 'application/json' } : {}), ...(token ? { Authorization: 'Bearer ' + token } : {}) },
            body: mutation ? JSON.stringify(value) : undefined, signal: controller.signal });
        } catch (error) { throw new RequestError('本机请求未取得响应；提交结果待核实。', 'transport'); }
        let body;
        try { body = await (raw && response.ok ? response.blob() : response.json()); }
        catch (error) { throw new RequestError('响应无法读取；提交结果待核实。', 'parse', response.status); }
        if (!response.ok) throw new RequestError(typeof body?.error === 'string' ? body.error : '本机请求失败', 'http', response.status,
          mutation && response.status >= 400 && response.status < 500 && body?.outcome === 'rejected');
        return body;
      })();
      try { return await Promise.race([cancelled, request]); }
      finally { clearTimeout(timer); if (onAbort) external.removeEventListener('abort', onAbort); }
    };
  }
  function createPoll({ read, success, failure, intervalMs = 1200, maxDelayMs = 15000 }) {
    let running = false, active = null, timer = null, generation = 0, failures = 0, awakened = false;
    function schedule(delay) {
      clearTimeout(timer);
      timer = setTimeout(() => { timer = null; run(); }, delay);
    }
    async function run() {
      if (!running || active) return;
      const expected = generation, request = new AbortController(); active = request;
      let delay = intervalMs;
      try {
        const value = await read({ signal: request.signal });
        if (running && expected === generation) { failures = 0; await success(value); }
      } catch (error) {
        if (running && expected === generation) {
          delay = Math.min(maxDelayMs, intervalMs * 2 ** Math.min(++failures, 20));
          if (error.category === 'http' && error.status === 403) running = false;
          failure(error);
        }
      } finally {
        active = null;
        if (running) schedule(expected !== generation || awakened && failures === 0 ? 0 : delay);
        awakened = false;
      }
    }
    return {
      start() { if (running) return; generation++; failures = 0; running = true; clearTimeout(timer); timer = null; if (!active) run(); },
      stop() { running = false; generation++; awakened = false; clearTimeout(timer); timer = null; if (active) active.abort(); },
      wake() { if (!running) return; if (active) awakened = true; else { clearTimeout(timer); timer = null; run(); } }
    };
  }
  function safeStorage(getStorage = () => globalThis.sessionStorage) {
    const memory = new Map(); let storage, available = true;
    try { storage = getStorage(); if (!storage) available = false; } catch (error) { available = false; }
    return {
      get available() { return available; },
      get(key) {
        if (available) {
          try { const value = storage.getItem(key); memory.set(key, value); return value; }
          catch (error) { available = false; }
        }
        return memory.get(key) ?? null;
      },
      set(key, value) {
        memory.set(key, value);
        if (available) { try { storage.setItem(key, value); return true; } catch (error) { available = false; } }
        return false;
      },
      remove(key) {
        memory.delete(key);
        if (available) { try { storage.removeItem(key); return true; } catch (error) { available = false; } }
        return false;
      }
    };
  }
  return { RequestError, createApi, createPoll, safeStorage };
});
