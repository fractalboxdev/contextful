// A call-mode delegate on a Cloudflare Worker (`surface.delegate`): the daemon posts one
// signed envelope per item attempt, the handler answers in the outcome vocabulary, and the
// answer goes back signed over the request's idempotency key. The engine's clauses are
// normative; every restatement here names its clause with a `mirrors:` comment.

/** The entity an isolated item runs under, with the fence its lease holds. */
export type Entity = { namespace: string; key: string; fence: number };

/** What one delegate request carries (`surface.delegate.envelope`). */
export type Envelope = {
  job: string;
  execution_id: string;
  step: string;
  input_hash: string;
  attempt: number;
  idempotency_key: string;
  entity?: Entity;
  as_of: string;
  /** Present only when the job fires from an applied snapshot. */
  version?: number;
  pins: Record<string, string>;
  tables: string[];
  row: Record<string, unknown>;
  deadline: string;
};

/** One row of one output table, as the engine lands it. */
export type Row = Record<string, unknown>;

/** A delegate's answer (`surface.delegate.outcome`). */
export type Answer =
  | { outcome: "ok"; rows: Record<string, Row[]> }
  | { outcome: "defer"; retry_at: string; reason?: string }
  | { outcome: "transient"; reason?: string }
  | { outcome: "permanent"; reason?: string };

export const ok = (rows: Record<string, Row[]>): Answer => ({ outcome: "ok", rows });
export const defer = (retryAt: Date | string, reason?: string): Answer => ({
  outcome: "defer",
  retry_at: typeof retryAt === "string" ? retryAt : retryAt.toISOString().replace(/\.\d{3}Z$/, "Z"),
  ...(reason === undefined ? {} : { reason }),
});
export const transient = (reason?: string): Answer => ({ outcome: "transient", ...(reason === undefined ? {} : { reason }) });
export const permanent = (reason?: string): Answer => ({ outcome: "permanent", ...(reason === undefined ? {} : { reason }) });

/** The header names the request and the answer carry. */
export const TIMESTAMP_HEADER = "x-contextful-timestamp";
export const SIGNATURE_HEADER = "x-contextful-signature";

// mirrors: surface.dispatch.callback-skew
/** Seconds a signature timestamp may differ from this side's system clock. */
export const SKEW_SECS = 300;

// mirrors: surface.dispatch.step-result
/** Largest answer body the engine admits, in bytes. */
export const ANSWER_CAP = 1024 * 1024;

/** Why a request was refused before the handler ran. */
export class DelegateRequestRejected extends Error {
  readonly status: 401 | 422;
  constructor(status: 401 | 422, message: string) {
    super(`DelegateRequestRejected: ${message}`);
    this.name = "DelegateRequestRejected";
    this.status = status;
  }
}

/** Why an answer the handler produced cannot be sent. */
export class DelegateAnswerInvalid extends Error {
  constructor(message: string) {
    super(`DelegateAnswerInvalid: ${message}`);
    this.name = "DelegateAnswerInvalid";
  }
}

const encoder = new TextEncoder();

async function hmacHex(key: string | Uint8Array, message: Uint8Array): Promise<string> {
  const raw = typeof key === "string" ? encoder.encode(key) : key;
  const cryptoKey = await crypto.subtle.importKey("raw", raw as BufferSource, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const signature = new Uint8Array(await crypto.subtle.sign("HMAC", cryptoKey, message as BufferSource));
  return Array.from(signature, (b) => b.toString(16).padStart(2, "0")).join("");
}

function concat(prefix: string, body: Uint8Array): Uint8Array {
  const head = encoder.encode(prefix);
  const out = new Uint8Array(head.length + body.length);
  out.set(head, 0);
  out.set(body, head.length);
  return out;
}

function constantEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

// mirrors: surface.delegate.signed
/** The request signature: HMAC-SHA256 under `key` over `call\n<timestamp>\n` and the body. */
export function signRequest(key: string | Uint8Array, timestamp: number, body: Uint8Array): Promise<string> {
  return hmacHex(key, concat(`call\n${timestamp}\n`, body));
}

// mirrors: surface.delegate.signed
/**
 * The answer signature: HMAC-SHA256 under `key` over `answer\n<idempotency_key>\n<timestamp>\n`
 * and the body, so an answer verifies only for the request it answers.
 */
export function signAnswer(key: string | Uint8Array, idempotencyKey: string, timestamp: number, body: Uint8Array): Promise<string> {
  return hmacHex(key, concat(`answer\n${idempotencyKey}\n${timestamp}\n`, body));
}

/** Seconds since the Unix epoch on this side's system clock (`surface.delegate.clocks`). */
export const systemNow = (): number => Math.floor(Date.now() / 1000);

/** The largest delay a JavaScript timer takes, in milliseconds. */
const TIMER_MAX_MS = 2 ** 31 - 1;

/**
 * Milliseconds the handler has before the engine closes the attempt, measured against the
 * envelope's own `deadline` rather than this side's clock, and clamped to what a timer
 * takes: the engine's instant may sit years from the wall clock under an injected `--now`.
 */
export function timeToDeadline(envelope: Envelope, reference: Date | string = envelope.as_of): number {
  const deadline = Date.parse(envelope.deadline);
  const from = typeof reference === "string" ? Date.parse(reference) : reference.getTime();
  if (Number.isNaN(deadline) || Number.isNaN(from)) return 0;
  return Math.min(Math.max(deadline - from, 0), TIMER_MAX_MS);
}

/**
 * Admit one delegate request: signed under `key` over its timestamp and body, the
 * timestamp within the skew of this side's system clock, the body an envelope. Refuses with
 * 401 for a signature or skew failure and 422 for a body that is no envelope.
 */
export async function verifyRequest(request: Request, key: string | Uint8Array, now: () => number = systemNow): Promise<Envelope> {
  const timestampHeader = request.headers.get(TIMESTAMP_HEADER);
  const signature = request.headers.get(SIGNATURE_HEADER);
  if (timestampHeader === null || signature === null) {
    throw new DelegateRequestRejected(401, `a request carries \`${TIMESTAMP_HEADER}\` and \`${SIGNATURE_HEADER}\``);
  }
  const timestamp = Number(timestampHeader);
  if (!Number.isInteger(timestamp)) {
    throw new DelegateRequestRejected(401, `\`${TIMESTAMP_HEADER}\` \`${timestampHeader}\` is no Unix second`);
  }
  const body = new Uint8Array(await request.arrayBuffer());
  if (!constantEqual(await signRequest(key, timestamp, body), signature)) {
    throw new DelegateRequestRejected(401, "the request's signature does not verify under the deployment key");
  }
  const skew = Math.abs(now() - timestamp);
  if (skew > SKEW_SECS) {
    throw new DelegateRequestRejected(401, `the request's timestamp is ${skew} s from this clock, past ${SKEW_SECS} s`);
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(body));
  } catch (error) {
    throw new DelegateRequestRejected(422, `the body is no envelope: ${(error as Error).message}`);
  }
  return envelopeOf(parsed);
}

function envelopeOf(value: unknown): Envelope {
  const malformed = (what: string) => new DelegateRequestRejected(422, `the body is no envelope: ${what}`);
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw malformed("not an object");
  const e = value as Record<string, unknown>;
  for (const field of ["job", "execution_id", "step", "input_hash", "idempotency_key", "as_of", "deadline"]) {
    if (typeof e[field] !== "string") throw malformed(`\`${field}\` is a string`);
  }
  if (!Number.isInteger(e.attempt)) throw malformed("`attempt` is an integer");
  if (e.version !== undefined && !Number.isInteger(e.version)) throw malformed("`version` is an integer when present");
  if (typeof e.pins !== "object" || e.pins === null) throw malformed("`pins` is an object");
  if (!Array.isArray(e.tables) || !e.tables.every((t) => typeof t === "string")) throw malformed("`tables` is a list of names");
  if (typeof e.row !== "object" || e.row === null) throw malformed("`row` is an object");
  if (e.entity !== undefined) {
    const entity = e.entity as Record<string, unknown>;
    if (typeof entity !== "object" || entity === null || typeof entity.namespace !== "string" || typeof entity.key !== "string" || !Number.isInteger(entity.fence)) {
      throw malformed("`entity` carries namespace, key and fence");
    }
  }
  return e as unknown as Envelope;
}

// mirrors: surface.delegate.outcome
function checkAnswer(envelope: Envelope, answer: Answer): void {
  switch (answer.outcome) {
    case "ok":
      for (const table of Object.keys(answer.rows)) {
        if (!envelope.tables.includes(table)) {
          throw new DelegateAnswerInvalid(`rows for \`${table}\`, which the job does not declare in \`tables\` (${envelope.tables.join(", ")})`);
        }
      }
      return;
    case "defer":
      if (Number.isNaN(Date.parse(answer.retry_at))) throw new DelegateAnswerInvalid(`\`retry_at\` \`${answer.retry_at}\` is no instant`);
      return;
    case "transient":
    case "permanent":
      return;
    default:
      throw new DelegateAnswerInvalid(`outcome is \`ok\`, \`defer\`, \`transient\` or \`permanent\``);
  }
}

/**
 * The signed response carrying `answer` to the request `envelope` came in. Refuses an answer
 * outside the vocabulary, naming an undeclared table, or over the engine's cap, before it
 * leaves.
 */
export async function respond(envelope: Envelope, answer: Answer, key: string | Uint8Array, now: () => number = systemNow): Promise<Response> {
  checkAnswer(envelope, answer);
  const body = encoder.encode(JSON.stringify(answer));
  if (body.length > ANSWER_CAP) {
    throw new DelegateAnswerInvalid(`an answer of ${body.length} B exceeds the ${ANSWER_CAP} B cap`);
  }
  const timestamp = now();
  return new Response(body, {
    status: 200,
    headers: {
      "content-type": "application/json",
      [TIMESTAMP_HEADER]: String(timestamp),
      [SIGNATURE_HEADER]: await signAnswer(key, envelope.idempotency_key, timestamp, body),
    },
  });
}

/** What a delegate handler does with one item: answer in the vocabulary. */
export type Handler<Env = unknown> = (envelope: Envelope, env: Env, request: Request) => Promise<Answer> | Answer;

export type DelegateOptions<Env> = {
  /** The deployment key, or how to read it from the Worker's environment. */
  key: string | Uint8Array | ((env: Env) => string | Uint8Array);
  /** This side's clock, in Unix seconds; the system clock unless a test injects one. */
  now?: () => number;
};

/**
 * A Worker `fetch` handler for a call-mode delegate: verify the request, run `handler`,
 * sign the answer. A refused request answers 401 or 422 with no handler run; a handler that
 * throws answers 500, which the engine closes Transient; an answer the handler produced
 * outside the contract answers 500 before it leaves.
 */
export function delegate<Env = unknown>(handler: Handler<Env>, options: DelegateOptions<Env>): (request: Request, env: Env) => Promise<Response> {
  const now = options.now ?? systemNow;
  return async (request, env) => {
    const key = typeof options.key === "function" ? options.key(env) : options.key;
    if (request.method !== "POST") {
      return problem(405, "a delegate takes `POST`");
    }
    let envelope: Envelope;
    try {
      envelope = await verifyRequest(request, key, now);
    } catch (error) {
      if (error instanceof DelegateRequestRejected) return problem(error.status, error.message);
      throw error;
    }
    let answer: Answer;
    try {
      answer = await handler(envelope, env, request);
    } catch (error) {
      return problem(500, `the handler failed: ${(error as Error).message ?? String(error)}`);
    }
    try {
      return await respond(envelope, answer, key, now);
    } catch (error) {
      if (error instanceof DelegateAnswerInvalid) return problem(500, error.message);
      throw error;
    }
  };
}

function problem(status: number, message: string): Response {
  return new Response(JSON.stringify({ error: message }), { status, headers: { "content-type": "application/json" } });
}
