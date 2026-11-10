import { DurableObject } from "cloudflare:workers";

const PORT = 8000;
const INSTANCE = "standard-1";
// Platform maximum. Traffic refreshes Durable Object activity; after a
// longer idle sleep the next request cold-starts from the R2-backed /data.
const INACTIVITY_TIMEOUT_MS = 6 * 60 * 60 * 1000;
const READY_ATTEMPTS = 120;
const READY_DELAY_MS = 500;

function rewriteForContainer(request) {
  const url = new URL(request.url);
  url.protocol = "http:";
  url.host = "container";
  const forwarded = new Request(url, request);
  forwarded.headers.delete("host");
  return forwarded;
}

export class NimProxyContainer extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    const container = ctx.container;
    if (container?.running) {
      ctx.blockConcurrencyWhile(async () => {
        await container.setInactivityTimeout(INACTIVITY_TIMEOUT_MS);
      });
    }
  }

  async fetch(request) {
    const container = this.ctx.container;
    if (!container) {
      return new Response("container is not configured", { status: 500 });
    }

    if (!container.running) {
      if (!this.env.AWS_ACCESS_KEY_ID || !this.env.AWS_SECRET_ACCESS_KEY) {
        return new Response("container storage secrets are not configured", {
          status: 503,
        });
      }
      container.start({
        image: container.images.base,
        instance: INSTANCE,
        enableInternet: true,
        env: {
          AWS_ACCESS_KEY_ID: this.env.AWS_ACCESS_KEY_ID,
          AWS_SECRET_ACCESS_KEY: this.env.AWS_SECRET_ACCESS_KEY,
          R2_BUCKET_NAME: this.env.R2_BUCKET_NAME,
          R2_ACCOUNT_ID: this.env.R2_ACCOUNT_ID,
          TRUST_PROXY: "true",
        },
      });
      this.ctx.waitUntil(
        container
          .monitor()
          .then(() => console.log("nim-proxy container exited"))
          .catch((error) => console.error("nim-proxy container error", error)),
      );
    }

    await container.setInactivityTimeout(INACTIVITY_TIMEOUT_MS);

    const port = container.getTcpPort(PORT);
    let ready = false;
    let lastError;
    for (let attempt = 0; attempt < READY_ATTEMPTS; attempt++) {
      try {
        const probe = await port.fetch("http://container/health");
        await probe.body?.cancel();
        if (probe.status === 200) {
          ready = true;
          break;
        }
        lastError = new Error(`health probe returned ${probe.status}`);
      } catch (error) {
        lastError = error;
      }
      await new Promise((resolve) => setTimeout(resolve, READY_DELAY_MS));
    }
    if (!ready) {
      console.error("nim-proxy container did not become ready", lastError);
      return new Response("container is starting; retry shortly", {
        status: 503,
        headers: { "retry-after": "5" },
      });
    }

    return port.fetch(rewriteForContainer(request));
  }
}

export default {
  async fetch(request, env) {
    // Exactly one named Durable Object: a second instance with the same
    // NIM keys would split the in-memory rate window and could exceed
    // the per-key upstream limit.
    return env.NIM_PROXY.getByName("nim-proxy-singleton").fetch(request);
  },
};
