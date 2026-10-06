import { describe, expect, it, vi } from "vitest";
import { apnsConfig, type Env } from "./env";
import { verifyToken } from "./auth";

const configured = {
  APNS_KEY_P8: "operator-key",
  APNS_KEY_ID: "operator-key-id",
  APNS_TEAM_ID: "operator-team",
  APNS_TOPIC: "invalid.example.operator-app"
};

describe("operator-owned deployment defaults", () => {
  it("requires every APNs identity field rather than falling back to upstream", () => {
    for (const missing of Object.keys(configured)) {
      const partial = { ...configured, [missing]: undefined } as unknown as Env;
      expect(apnsConfig(partial)).toBeUndefined();
    }
    expect(apnsConfig(configured as unknown as Env)).toEqual({
      keyP8: configured.APNS_KEY_P8,
      keyId: configured.APNS_KEY_ID,
      teamId: configured.APNS_TEAM_ID,
      topic: configured.APNS_TOPIC
    });
  });

  it("an unconfigured/unknown auth mode rejects tokens without calling any tenant", async () => {
    const fetch = vi.spyOn(globalThis, "fetch");
    try {
      for (const AUTH_MODE of ["workos", "", "unknown"]) {
        expect(await verifyToken({ AUTH_MODE, WORKOS_CLIENT_ID: "" } as Env, "dummy")).toBeUndefined();
      }
      expect(fetch).not.toHaveBeenCalled();
    } finally {
      fetch.mockRestore();
    }
  });

  it("explicit local development auth remains available for fixtures", async () => {
    expect(await verifyToken({ AUTH_MODE: "dev" } as Env, "alice@org1")).toEqual({ userId: "alice", orgId: "org1" });
    expect(await verifyToken({ AUTH_MODE: "dev" } as Env, "")).toBeUndefined();
  });
});
