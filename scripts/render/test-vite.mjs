import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "vite";

export async function startTestVite(config) {
  const cacheDir = await mkdtemp(join(tmpdir(), "philium-vite-test-"));
  let vite;
  const close = async () => {
    try {
      await vite?.close();
    } finally {
      await rm(cacheDir, { recursive: true, force: true });
    }
  };
  try {
    vite = await createServer({ ...config, cacheDir, server: { host: "127.0.0.1", hmr: false } });
    const server = vite.httpServer;
    await new Promise((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", resolve);
    });
    return { url: `http://127.0.0.1:${server.address().port}/`, close };
  } catch (error) {
    await close();
    throw error;
  }
}
