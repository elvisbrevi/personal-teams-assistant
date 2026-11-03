import { cloudflareTest } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

// Tests run inside the Workers runtime (Miniflare) with the production configuration and
// synthetic Access settings; nothing reaches Cloudflare and no secret is needed.
export default defineConfig({
  plugins: [
    cloudflareTest({
      wrangler: { configPath: "./wrangler.jsonc" },
      miniflare: {
        bindings: {
          ACCESS_TEAM_DOMAIN: "https://synthetic.cloudflareaccess.com",
          ACCESS_AUD: "synthetic-audience",
          ACCOUNT_DAILY_REQUESTS: "50",
          ACCOUNT_DAILY_ROWS_WRITTEN: "40",
        },
      },
    }),
  ],
});
