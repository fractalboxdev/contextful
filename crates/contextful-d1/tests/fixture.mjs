import { createServer } from "node:http";
import { DatabaseSync } from "node:sqlite";
import { readFileSync, writeFileSync, mkdirSync, existsSync, unlinkSync } from "node:fs";
import { join } from "node:path";

const root = process.argv[2];
const db = new DatabaseSync(join(root, "execution.sqlite"));
mkdirSync(join(root, "blobs"), { recursive: true });

function execute({ sql, params }) {
  const query = db.prepare(sql);
  const rows = query.columns().length ? query.all(...params) : (query.run(...params), []);
  return { rows, changes: Number(db.prepare("SELECT changes() AS n").get().n) };
}

// Every request served, by path; `GET /stats` answers them and counts itself in none.
const requests = { total: 0, paths: {} };

const server = createServer(async (request, response) => {
  try {
    const path = new URL(request.url, "http://contextful.d1").pathname;
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = Buffer.concat(chunks);
    if (path === "/stats" && request.method === "GET") {
      response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(requests));
      return;
    }
    requests.total += 1;
    requests.paths[path] = (requests.paths[path] ?? 0) + 1;
    let value;
    if (path === "/health") {
      value = { d1: true, r2: true };
    } else if (path === "/sql") {
      value = execute(JSON.parse(body));
    } else if (path === "/batch") {
      db.exec("BEGIN IMMEDIATE");
      try {
        value = JSON.parse(body).map(execute);
        db.exec("COMMIT");
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      }
    } else if (/^\/blob\/[a-f0-9]{64}$/.test(path)) {
      const file = join(root, "blobs", path.slice(6));
      if (request.method === "PUT") writeFileSync(file, body);
      if (request.method === "DELETE" && existsSync(file)) unlinkSync(file);
      if (request.method === "HEAD") {
        response.writeHead(existsSync(file) ? 204 : 404).end();
        return;
      }
      if (request.method === "GET") {
        if (!existsSync(file)) {
          response.writeHead(404).end();
          return;
        }
        response.writeHead(200).end(readFileSync(file));
        return;
      }
      response.writeHead(204).end();
      return;
    } else {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(value));
  } catch (error) {
    response.writeHead(503).end(String(error));
  }
});
server.listen(0, "127.0.0.1", () => console.log(server.address().port));
