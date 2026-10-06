export default {
  async fetch(request, env) {
    let target;
    try {
      target = new URL(env.CANONICAL_ORIGIN);
      if (target.protocol !== "https:" || target.username || target.password ||
          target.pathname !== "/" || target.search || target.hash) throw new Error("invalid origin");
    } catch {
      return new Response("Set CANONICAL_ORIGIN to your own HTTPS origin before deployment.", { status: 503 });
    }
    const url = new URL(request.url);
    if (url.origin === target.origin) return new Response("Redirect loop prevented.", { status: 508 });
    target.pathname = url.pathname;
    target.search = url.search;
    return Response.redirect(target.href, 301);
  }
};
