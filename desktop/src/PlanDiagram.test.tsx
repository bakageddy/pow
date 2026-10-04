// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest"
import { render } from "solid-js/web"
import { createSignal } from "solid-js"
import type { IPlan } from "@/interfaces"
import { PlanDiagram } from "./PlanDiagram"

class ResizeObserverStub {
  observe() {}
  disconnect() {}
}
vi.stubGlobal("ResizeObserver", ResizeObserverStub)

describe("PEV2-style plan canvas", () => {
  it("lays out PostgreSQL nodes and connects them using d3-flextree", () => {
    const plan = {
      id: "p1", isAnalyze: true, ctes: [],
      content: { maxRows: 100, maxDuration: 12, maxCost: 100, Plan: {
        nodeId: 1, "Node Type": "Nested Loop", "*Duration (exclusive)": 2,
        Plans: [
          { nodeId: 2, "Node Type": "Index Scan", "Relation Name": "orders", "*Duration (exclusive)": 7, "*Actual Rows Revised": 100, Plans: [] },
          { nodeId: 3, "Node Type": "Seq Scan", "Relation Name": "customers", "*Duration (exclusive)": 3, Plans: [] },
        ],
      } },
    } as unknown as IPlan
    const root = document.createElement("div")
    document.body.append(root)
    const dispose = render(() => {
      const [selected, setSelected] = createSignal<number | null>(null)
      return <PlanDiagram plan={plan} selected={selected()} onSelect={node => setSelected(node?.nodeId || null)} />
    }, root)
    expect(root.querySelectorAll("foreignObject")).toHaveLength(3)
    expect(root.querySelectorAll("path.diagram-edge")).toHaveLength(2)
    expect(root.textContent).toContain("orders")
    expect(root.textContent).toContain("Nested Loop")
    const card = [...root.querySelectorAll("[data-plan-card]")].find(el => el.textContent?.includes("orders")) as HTMLElement
    card.click()
    expect(root.querySelector(".canvas-inspector")?.textContent).toContain("orders")
    ;(root.querySelector(".canvas-inspector-head button") as HTMLButtonElement).click()
    expect(root.querySelector(".canvas-inspector")).toBeNull()
    dispose()
    root.remove()
  })
})
