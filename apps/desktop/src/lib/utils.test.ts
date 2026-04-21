import { describe, expect, it } from "vitest";

import { clampThreshold, eventTone, groupImpactEntries } from "./utils";

describe("groupImpactEntries", () => {
  it("groups reverse dependencies by depth", () => {
    const grouped = groupImpactEntries([
      { path: "/a.rs", kind: "imports", depth: 2 },
      { path: "/b.rs", kind: "imports", depth: 1 },
      { path: "/c.rs", kind: "references", depth: 2 },
    ]);

    expect(grouped[1]).toHaveLength(1);
    expect(grouped[2]).toHaveLength(2);
  });
});

describe("eventTone", () => {
  it("maps known history events to stable badge tones", () => {
    expect(eventTone("created")).toBe("success");
    expect(eventTone("deleted")).toBe("danger");
  });
});

describe("clampThreshold", () => {
  it("keeps duplicate thresholds within valid bounds", () => {
    expect(clampThreshold(-1)).toBe(0);
    expect(clampThreshold(0.97)).toBe(0.97);
    expect(clampThreshold(2)).toBe(1);
  });
});
