const VERSION = '1';

function b64urlToBytes(s: string): Uint8Array {
  const pad = '==='.slice((s.length + 3) % 4);
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/') + pad;
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

function bytesToB64url(bytes: Uint8Array): string {
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

async function gunzip(bytes: Uint8Array): Promise<Uint8Array> {
  if (typeof DecompressionStream !== 'undefined' && typeof Blob !== 'undefined') {
    try {
      // Slice to guarantee a plain ArrayBuffer (cast avoids SharedArrayBuffer TS error)
      const buf = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
      const stream = new Blob([buf]).stream().pipeThrough(new DecompressionStream('gzip'));
      return new Uint8Array(await new Response(stream).arrayBuffer());
    } catch {
      // fall through to fflate
    }
  }
  const { gunzipSync } = await import('fflate');
  return gunzipSync(bytes);
}

async function gzip(bytes: Uint8Array): Promise<Uint8Array> {
  if (typeof CompressionStream !== 'undefined' && typeof Blob !== 'undefined') {
    try {
      // Slice to guarantee a plain ArrayBuffer (cast avoids SharedArrayBuffer TS error)
      const buf = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
      const stream = new Blob([buf]).stream().pipeThrough(new CompressionStream('gzip'));
      return new Uint8Array(await new Response(stream).arrayBuffer());
    } catch {
      // fall through to fflate
    }
  }
  const { gzipSync } = await import('fflate');
  return gzipSync(bytes);
}

/** Decode `#1.<base64url-gzip-json>` into a parsed (unvalidated) value. Throws on bad input. */
export async function decodeFragment(hash: string): Promise<unknown> {
  const frag = hash.startsWith('#') ? hash.slice(1) : hash;
  const dot = frag.indexOf('.');
  if (dot < 0) throw new Error('malformed diagram link');
  const version = frag.slice(0, dot);
  if (version !== VERSION) throw new Error(`unsupported diagram link version "${version}"`);
  const gz = b64urlToBytes(frag.slice(dot + 1));
  const json = new TextDecoder().decode(await gunzip(gz));
  return JSON.parse(json);
}

/** Encode a value into a `1.<base64url-gzip-json>` payload (no leading `#`). */
export async function encodeFragment(model: unknown): Promise<string> {
  const json = new TextEncoder().encode(JSON.stringify(model));
  return `${VERSION}.${bytesToB64url(await gzip(json))}`;
}
