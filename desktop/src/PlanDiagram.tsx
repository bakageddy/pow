import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, type Component } from "solid-js"
import * as d3 from "d3"
import { flextree, type FlexHierarchyPointNode } from "d3-flextree"
import type { IPlan, Node } from "@/interfaces"

const CARD_WIDTH = 240
const NODE_GAP = 40
const CARD_HEIGHT_BASE = 118
const CARD_HEIGHT_EXPANDED = 198
const cardHeight = (expanded: boolean) => expanded ? CARD_HEIGHT_EXPANDED : CARD_HEIGHT_BASE

const numeric = (node: Node, key: string): number | undefined => {
  const v = node[key]
  return typeof v === "number" && Number.isFinite(v) ? v : undefined
}
const formatted = (v: number | undefined, unit = "") =>
  v === undefined ? "—" : `${new Intl.NumberFormat("en", { maximumFractionDigits: 2 }).format(v)}${unit}`

const nodeTitle = (node: Node) =>
  [node["Parallel Aware"] ? "Parallel" : "", node["Partial Mode"] || "", node["Node Type"] || "Operation"]
    .filter(Boolean).join(" ")

type Point = FlexHierarchyPointNode<Node>
type Edge = { source: Point; target: Point; cte?: boolean }
type Box = { x: number; y: number; width: number; height: number; label: string }

function extent(nodes: Point[], h: (p: Point) => number) {
  return {
    left: Math.min(...nodes.map(p => p.x - CARD_WIDTH / 2)),
    right: Math.max(...nodes.map(p => p.x + CARD_WIDTH / 2)),
    top: Math.min(...nodes.map(p => p.y)),
    bottom: Math.max(...nodes.map(p => p.y + h(p))),
  }
}

export const PlanDiagram: Component<{ plan: IPlan; selected: number | null; onSelect: (node: Node | null) => void }> = props => {
  let canvas!: SVGSVGElement
  let viewport!: HTMLDivElement
  const [expanded, setExpanded] = createSignal<Set<number>>(new Set())
  const [metric, setMetric] = createSignal<"duration" | "cost" | "rows">(props.plan.isAnalyze ? "duration" : "cost")
  const [transform, setTransform] = createSignal(d3.zoomIdentity)
  const [scale, setScale] = createSignal(1)

  const isExpanded = (id: number) => expanded().has(id)
  const h = (point: Point) => cardHeight(isExpanded(point.data.nodeId))

  const diagram = createMemo(() => {
    const layout = flextree<Node>({
      nodeSize: node => [CARD_WIDTH + 20, cardHeight(isExpanded(node.data.nodeId)) + NODE_GAP],
      spacing: (a, b) => Math.pow(a.path(b).length, 1.5),
    })
    const main = layout(layout.hierarchy(props.plan.content.Plan, node => node.Plans || []))
    const nodes: Point[] = [...main.descendants()]
    const edges: Edge[] = main.links()
    const boxes: Box[] = []
    const mainExtent = extent(nodes, h)
    let nextX = mainExtent.left
    const cteRoots: Point[] = []
    for (const cte of props.plan.ctes || []) {
      const root = layout(layout.hierarchy(cte, node => node.Plans || []))
      const descendants = root.descendants()
      const box = extent(descendants, h)
      const shiftX = nextX - box.left + 15
      const shiftY = mainExtent.bottom - box.top + 100
      descendants.forEach(node => { node.x += shiftX; node.y += shiftY })
      nodes.push(...descendants)
      edges.push(...root.links())
      boxes.push({ x: box.left + shiftX - 12, y: box.top + shiftY - 30, width: box.right - box.left + 24, height: box.bottom - box.top + 44, label: String(cte["Subplan Name"] || "CTE") })
      cteRoots.push(root)
      nextX += box.right - box.left + 75
    }
    for (const node of nodes) {
      const name = node.data["CTE Name"]
      if (typeof name === "string") {
        const target = cteRoots.find(root => root.data["Subplan Name"] === `CTE ${name}`)
        if (target) edges.push({ source: node, target, cte: true })
      }
    }
    const bounds = extent(nodes, h)
    return { nodes, edges, boxes, bounds }
  })

  const inspected = createMemo(() => diagram().nodes.find(p => p.data.nodeId === props.selected)?.data)
  const zoom = d3.zoom<SVGSVGElement, unknown>()
    .scaleExtent([0.2, 3])
    .filter(event => event.type === "wheel" || (event.button === 0 && !(event.target as Element).closest("[data-plan-card]")))
    .on("zoom", event => { setTransform(event.transform); setScale(event.transform.k) })

  function fit() {
    if (!canvas || !viewport) return
    const { left, right, top, bottom } = diagram().bounds
    const vw = viewport.clientWidth
    const vh = viewport.clientHeight
    if (!vw || !vh) return
    const k = Math.max(.2, Math.min(1.3, .86 * vw / Math.max(1, right - left), .86 * vh / Math.max(1, bottom - top)))
    const cx = (left + right) / 2
    const cy = (top + bottom) / 2
    d3.select(canvas).call(zoom.transform, d3.zoomIdentity.translate(vw / 2 - cx * k, vh / 2 - cy * k).scale(k))
  }
  function zoomBy(f: number) { d3.select(canvas).transition().duration(180).call(zoom.scaleBy, f) }
  function toggle(id: number) {
    setExpanded(old => { const s = new Set(old); s.has(id) ? s.delete(id) : s.add(id); return s })
  }

  const value = (node: Node) => {
    if (metric() === "cost") return numeric(node, "*Cost (exclusive)") ?? numeric(node, "Total Cost")
    if (metric() === "rows") return numeric(node, "*Actual Rows Revised") ?? numeric(node, "Plan Rows")
    return numeric(node, "*Duration (exclusive)")
  }
  const maxMetric = () => {
    if (metric() === "duration") return props.plan.content.maxDuration || 0
    if (metric() === "cost") return props.plan.content.maxCost || 0
    return Math.max(...diagram().nodes.map(p => value(p.data) || 0), 1)
  }
  const barPct = (node: Node) => Math.min(100, Math.max(0, (value(node) || 0) / (maxMetric() || 1) * 100))
  const barColor = (pct: number) => pct >= 65 ? "var(--red)" : pct >= 30 ? "var(--accent)" : "var(--muted)"

  const edgePath = (edge: Edge) => {
    const fromY = edge.source.y + h(edge.source)
    const toY = edge.target.y
    const curve = d3.path()
    curve.moveTo(edge.source.x, fromY)
    curve.bezierCurveTo(edge.source.x, fromY + (toY - fromY) / 2, edge.target.x, toY - (toY - fromY) / 2, edge.target.x, toY)
    return curve.toString()
  }
  const edgeWidth = (node: Node) => {
    const max = props.plan.content.maxRows || 0
    const rows = numeric(node, "*Actual Rows Revised") || 0
    return max > 0 ? Math.max(1, Math.min(18, rows / max * 18)) : 1.3
  }

  onMount(() => {
    d3.select(canvas).call(zoom)
    const observer = new ResizeObserver(() => fit())
    observer.observe(viewport)
    requestAnimationFrame(fit)
    onCleanup(() => { observer.disconnect(); d3.select(canvas).on(".zoom", null) })
  })
  createEffect(() => { props.plan.id; requestAnimationFrame(fit) })
  createEffect(() => { props.plan.id; setMetric(props.plan.isAnalyze ? "duration" : "cost"); setExpanded(new Set<number>()) })

  return <div class="diagram-shell">
    <div class="diagram-toolbar">
      <div><strong>Execution graph</strong><span>Drag to pan · scroll to zoom</span></div>
      <div class="diagram-controls">
        <div class="metric-switch">
          <For each={["duration", "rows", "cost"] as const}>{item =>
            <button class={metric() === item ? "active" : ""} disabled={item === "duration" && !props.plan.isAnalyze} onClick={() => setMetric(item)}>{item}</button>
          }</For>
        </div>
        <button aria-label="Zoom out" title="Zoom out" onClick={() => zoomBy(.8)}>−</button>
        <span class="zoom-label">{Math.round(scale() * 100)}%</span>
        <button aria-label="Zoom in" title="Zoom in" onClick={() => zoomBy(1.25)}>+</button>
        <button title="Fit diagram" onClick={fit}>Fit</button>
      </div>
    </div>
    <div class="diagram-viewport" ref={viewport}>
      <svg ref={canvas} role="img" aria-label="Interactive PostgreSQL execution plan diagram" width="100%" height="100%">
        <g transform={transform().toString()}>
          <For each={diagram().boxes}>{box =>
            <g>
              <rect class="cte-outline" x={box.x} y={box.y} width={box.width} height={box.height} rx="3" />
              <text class="cte-label" x={box.x + 10} y={box.y + 17}>{box.label}</text>
            </g>
          }</For>
          <For each={diagram().edges}>{edge =>
            <path class={edge.cte ? "diagram-edge cte-edge" : "diagram-edge"} d={edgePath(edge)} stroke-width={edge.cte ? 1.5 : edgeWidth(edge.target.data)} />
          }</For>
          <For each={diagram().nodes}>{point => {
            const node = point.data
            const pct = barPct(node)
            const duration = numeric(node, "*Duration (exclusive)")
            const estimateFactor = numeric(node, "*Planner Row Estimate Factor")
            const exclusiveCost = numeric(node, "*Cost (exclusive)") || 0
            const maxCost = props.plan.content.maxCost || Infinity
            const workers = typeof node["Workers Planned"] === "number" ? (node["Workers Planned"] as number) : 0
            const neverExecuted = numeric(node, "Actual Loops") === 0 && numeric(node, "Actual Rows") === 0
            return <foreignObject x={point.x - CARD_WIDTH / 2} y={point.y} width={CARD_WIDTH} height={h(point)}>
              <div data-plan-card
                class={`diagram-card${props.selected === node.nodeId ? " selected" : ""}${neverExecuted ? " never-executed" : ""}`}
                onMouseDown={e => e.stopPropagation()}
                onClick={() => props.onSelect(node)}
              >
                <Show when={workers > 0}>
                  <For each={[...Array(Math.min(workers, 3)).keys()]}>{i =>
                    <div class="worker-stack" style={{ top: `${(i + 1) * 2 + 1}px`, left: `${(i + 1) * 3 + 1}px` }} />
                  }</For>
                </Show>
                <div class="diagram-card-inner">
                  <header class="diagram-card-head">
                    <h4 class="node-type-name" onClick={e => { e.stopPropagation(); toggle(node.nodeId) }}>
                      <span class="expand-chevron">{isExpanded(node.nodeId) ? "▴" : "▾"}</span>
                      {nodeTitle(node)}
                      <Show when={neverExecuted}><span class="badge-never"> (never)</span></Show>
                    </h4>
                    <div class="node-right">
                      <Show when={exclusiveCost > maxCost * .4}>
                        <span class="node-badge" title="High relative cost">$</span>
                      </Show>
                      <Show when={estimateFactor !== undefined && estimateFactor >= 10}>
                        <span class="node-badge warn" title={`Row estimate off by ${formatted(estimateFactor)}×`}>!</span>
                      </Show>
                      <Show when={workers > 0}>
                        <span class="node-badge workers-badge" title={`${workers} workers planned`}>×{workers}</span>
                      </Show>
                      <span class="node-id">#{node.nodeId}</span>
                    </div>
                  </header>
                  <div class="diagram-mono-detail">
                    <Show when={node["Relation Name"] || node["Function Name"]}>
                      <div class="mono-row">
                        <span class="kw">on</span>{" "}
                        {node["Schema"] ? `${String(node["Schema"])}.` : ""}
                        {String(node["Relation Name"] || node["Function Name"] || "")}
                        <Show when={node["Alias"] && node["Alias"] !== node["Relation Name"]}>
                          {" "}<span class="kw">as</span>{" "}{String(node["Alias"] || "")}
                        </Show>
                      </div>
                    </Show>
                    <Show when={node["Index Name"]}>
                      <div class="mono-row"><span class="kw">using</span>{" "}{String(node["Index Name"] || "")}</div>
                    </Show>
                    <Show when={!node["Relation Name"] && !node["Function Name"] && node["Alias"]}>
                      <div class="mono-row"><span class="kw">on</span>{" "}{String(node["Alias"] || "")}</div>
                    </Show>
                    <Show when={node["CTE Name"]}>
                      <div class="mono-row"><span class="kw">CTE</span>{" "}{String(node["CTE Name"] || "")}</div>
                    </Show>
                    <Show when={node["Group Key"]}>
                      <div class="mono-row"><span class="kw">by</span>{" "}{String(Array.isArray(node["Group Key"]) ? (node["Group Key"] as string[]).join(", ") : node["Group Key"] || "")}</div>
                    </Show>
                    <Show when={!node["Group Key"] && node["Sort Key"]}>
                      <div class="mono-row"><span class="kw">by</span>{" "}{String(Array.isArray(node["Sort Key"]) ? (node["Sort Key"] as string[]).join(", ") : node["Sort Key"] || "")}</div>
                    </Show>
                  </div>
                  <div class="diagram-track">
                    <div class="diagram-bar" style={{ width: `${pct}%`, background: barColor(pct) }} />
                  </div>
                  <div class="node-bar-label">
                    <span class="kw">{metric()}:</span>{" "}
                    <strong>{formatted(value(node), metric() === "duration" ? " ms" : "")}</strong>
                    <Show when={duration !== undefined && metric() !== "duration"}>
                      <span class="bar-secondary">{" · "}{formatted(duration, " ms")}</span>
                    </Show>
                  </div>
                  <Show when={isExpanded(node.nodeId)}>
                    <div class="card-expanded">
                      <div><span class="kw">Plan rows</span><b>{formatted(numeric(node, "Plan Rows"))}</b></div>
                      <div><span class="kw">Actual rows</span><b>{formatted(numeric(node, "Actual Rows"))}</b></div>
                      <div><span class="kw">Loops</span><b>{formatted(numeric(node, "Actual Loops"))}</b></div>
                      <Show when={estimateFactor !== undefined}>
                        <div><span class="kw">Est. factor</span><b>{formatted(estimateFactor)}×</b></div>
                      </Show>
                    </div>
                  </Show>
                </div>
              </div>
            </foreignObject>
          }}</For>
        </g>
      </svg>
      <Show when={inspected()}>{node =>
        <div class="canvas-inspector" onMouseDown={e => e.stopPropagation()}>
          <div class="canvas-inspector-head">
            <strong>{nodeTitle(node())} <small>#{node().nodeId}</small></strong>
            <button aria-label="Close node details" onClick={() => props.onSelect(null)}>×</button>
          </div>
          <div class="canvas-inspector-fields">
            <For each={Object.entries(node()).filter(([key, value]) =>
              !key.startsWith("*") && !["Plans", "Workers", "nodeId", "size"].includes(key) && value !== null && typeof value !== "object"
            )}>
              {([key, value]) => <div><span>{key}</span><strong>{String(value)}</strong></div>}
            </For>
          </div>
        </div>
      }</Show>
    </div>
  </div>
}
