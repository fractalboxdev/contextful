import { StrictMode } from "react";
import { renderToString } from "react-dom/server";
import { AdminApp } from "./admin.tsx";
import { QueryApp } from "./query.tsx";

/** The first paint each page hydrates over, rendered once at build time. */
export function render(page: "query" | "admin"): string {
  return renderToString(<StrictMode>{page === "admin" ? <AdminApp /> : <QueryApp />}</StrictMode>);
}
