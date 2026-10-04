export const themes = [
  { id: "gruvbox", name: "Gruvbox", swatches: ["#282828", "#d79921", "#b8bb26"] },
  { id: "nord", name: "Nord", swatches: ["#2e3440", "#88c0d0", "#a3be8c"] },
  { id: "modus-operandi", name: "Modus Operandi", swatches: ["#ffffff", "#0031a9", "#006800"] },
  { id: "modus-vivendi", name: "Modus Vivendi", swatches: ["#000000", "#79a8ff", "#44bc44"] },
  { id: "one-dark", name: "One Dark", swatches: ["#282c34", "#61afef", "#98c379"] },
  { id: "kanagawa", name: "Kanagawa", swatches: ["#1f1f28", "#7e9cd8", "#98bb6c"] },
  { id: "kanso", name: "Kanso", swatches: ["#14171d", "#8ba4b0", "#8a9a7b"] },
  { id: "rose-pine", name: "Rosé Pine", swatches: ["#191724", "#c4a7e7", "#9ccfd8"] },
  { id: "solarized-dark", name: "Solarized Dark", swatches: ["#002b36", "#268bd2", "#859900"] },
  { id: "solarized-light", name: "Solarized Light", swatches: ["#fdf6e3", "#268bd2", "#859900"] },
  { id: "github-dark", name: "GitHub Dark", swatches: ["#0d1117", "#58a6ff", "#3fb950"] },
  { id: "github-light", name: "GitHub Light", swatches: ["#ffffff", "#0969da", "#1a7f37"] },
] as const

export type ThemeId = (typeof themes)[number]["id"]
export function isThemeId(id: string): id is ThemeId {
  return themes.some(theme => theme.id === id)
}
