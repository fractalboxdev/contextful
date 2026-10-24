export const redirectLegacy = (request: Request): Response => {
  const url = new URL(request.url);
  url.protocol = "https:";
  url.hostname = "contextful.fractalbox.dev";
  url.port = "";
  if (url.pathname === "/") url.pathname = "/docs/specs/";
  return Response.redirect(url.toString(), 301);
};

export default {
  fetch(request: Request, env: Env): Promise<Response> | Response {
    if (new URL(request.url).hostname === "spec.contextful.work") return redirectLegacy(request);
    return env.ASSETS.fetch(request);
  },
};
