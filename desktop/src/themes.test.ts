import { describe, expect, it } from "vitest"
import { isThemeId, themes } from "./themes"

describe("theme catalog", () => {
  it("provides the requested families, including light and dark choices", () => {
    expect(themes.map(t => t.id)).toEqual([
      "gruvbox", "nord", "modus-operandi", "modus-vivendi", "one-dark", "kanagawa",
      "kanso", "rose-pine", "solarized-dark", "solarized-light", "github-dark", "github-light",
    ])
    expect(isThemeId("github-light")).toBe(true)
    expect(isThemeId("invalid")).toBe(false)
  })
})
