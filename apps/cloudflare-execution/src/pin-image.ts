/** The instance type every start of the execution container requests. */
export const CONTAINER_INSTANCE = "standard-1" satisfies ContainerStartupOptions["instance"];

/**
 * A Durable Object-managed container application names its images in `ctx.container.images`
 * and takes the image and instance type at each start; `@cloudflare/containers` passes
 * neither, so every start this object makes carries the first image and `instance`.
 */
export function pinImage(runtime: DurableObjectState["container"], instance: ContainerStartupOptions["instance"]): void {
  let images: Record<string, string> = {};
  try {
    images = runtime?.images ?? {};
  } catch {
    return;
  }
  const image = Object.values(images)[0];
  if (!runtime || !image) return;
  const start = runtime.start.bind(runtime);
  Object.defineProperty(runtime, "start", {
    configurable: true,
    writable: true,
    value: (options?: ContainerStartupOptions) => start({ enableInternet: true, ...options, image, instance } as ContainerStartupOptions),
  });
}
