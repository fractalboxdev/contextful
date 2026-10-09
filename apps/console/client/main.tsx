import { StrictMode } from "react";
import { hydrateRoot } from "react-dom/client";
import { AdminApp } from "./admin.tsx";
import { QueryApp } from "./query.tsx";

const root = document.getElementById("root");
if (root) hydrateRoot(root, <StrictMode>{root.dataset.page === "admin" ? <AdminApp /> : <QueryApp />}</StrictMode>);
