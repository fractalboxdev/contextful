import { Container } from "@cloudflare/containers";
import { handleExecutionBinding, type ExecutionBindings } from "./binding.ts";
import { admitConformanceRun, completeConformanceReport, type ReleaseProvenance } from "./conformance.ts";
import { CONTAINER_INSTANCE, pinImage } from "./pin-image.ts";

/** The Container's outbound virtual host resolves D1 and R2 inside its Worker. */
export class D1ExecutionContainer extends Container<ContextfulExecutionEnv> {
  private conformanceRunning = false;

  constructor(ctx: DurableObjectState<{}>, env: ContextfulExecutionEnv) {
    pinImage(ctx.container, CONTAINER_INSTANCE);
    super(ctx, env);
  }

  async runConformance() {
    if (this.conformanceRunning) throw new Error("ConformanceAlreadyRunning");
    this.conformanceRunning = true;
    try {
      return await this.executeConformance();
    } finally {
      this.conformanceRunning = false;
    }
  }

  private async executeConformance() {
    if (![
      this.env.CONFORMANCE_R2_ENDPOINT,
      this.env.CONFORMANCE_R2_BUCKET,
      this.env.CONFORMANCE_R2_ACCESS_KEY_ID,
      this.env.CONFORMANCE_R2_SECRET_ACCESS_KEY,
      this.env.CONFORMANCE_RECOVERY_ID,
    ].every((value) => typeof value === "string" && value.length > 0)) {
      throw new Error("ConformanceCapabilityMissing");
    }
    const container = this.ctx.container;
    if (!container) throw new Error("ConformanceContainerUnavailable");
    if (!container.running) await this.start();
    await admitConformanceRun(async (argv) => ({ exitCode: (await (await container.exec(argv)).output()).exitCode }));
    const reportPath = `/work/conformance-${crypto.randomUUID()}.json`;
    const process = await container.exec([
      "/usr/local/bin/contextful-conformance",
      "--binary", "/usr/local/bin/contextful",
      "--storage-profile", "cloudflare-d1",
      "--output", reportPath,
    ], { env: {
      CONTEXTFUL_CONFORMANCE_D1_ENDPOINT: "http://contextful.d1",
      CONTEXTFUL_CONFORMANCE_D1_DISPOSABLE: "1",
      CONTEXTFUL_CONFORMANCE_R2_ENDPOINT: this.env.CONFORMANCE_R2_ENDPOINT,
      CONTEXTFUL_CONFORMANCE_R2_BUCKET: this.env.CONFORMANCE_R2_BUCKET,
      CONTEXTFUL_CONFORMANCE_R2_ACCESS_KEY_ID: this.env.CONFORMANCE_R2_ACCESS_KEY_ID,
      CONTEXTFUL_CONFORMANCE_R2_SECRET_ACCESS_KEY: this.env.CONFORMANCE_R2_SECRET_ACCESS_KEY,
      CONTEXTFUL_CONFORMANCE_RECOVERY_ID: this.env.CONFORMANCE_RECOVERY_ID,
    } });
    const execution = await process.output();
    const read = async (path: string) => {
      const command = await container.exec(["/bin/cat", path]);
      const output = await command.output();
      if (output.exitCode !== 0 || output.stdout.byteLength > 65_536) {
        throw new Error("ConformanceReportUnavailable");
      }
      return JSON.parse(new TextDecoder().decode(output.stdout));
    };
    const report = await read(reportPath);
    const source = await read("/etc/contextful-source.json") as ReleaseProvenance;
    const complete = completeConformanceReport(report, source);
    return {
      ...complete,
      suite_exit_code: execution.exitCode,
      compatible: complete.compatible && execution.exitCode === 0,
    };
  }
}

D1ExecutionContainer.outboundByHost = {
  "contextful.d1": (request: Request, env: unknown) =>
    handleExecutionBinding(request, env as ExecutionBindings),
};
