export default {
  fetch(request: Request, env: Env): Promise<Response> | Response {
    const url = new URL(request.url);
    if (url.hostname === "spec.contextful.work") {
      url.protocol = "https:";
      url.hostname = "contextful.fractalbox.dev";
      url.port = "";
      if (url.pathname === "/") url.pathname = "/docs/specs/";
      return Response.redirect(url.toString(), 301);
    }
    return env.ASSETS.fetch(request);
  },
};
