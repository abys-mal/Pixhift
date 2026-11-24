import { getVersion } from "@tauri-apps/api/app"
import { Channel, invoke } from "@tauri-apps/api/core"
import { getCurrentWebview } from "@tauri-apps/api/webview"

type Page = "home" | "preview" | "processing" | "results" | "settings" | "advanced"
type Theme = "system" | "light" | "dark"
type Destination = "folder" | "alongside" | "overwrite"
type Format = "jpeg" | "png" | "webp" | "heic" | "avif" | "tiff" | "bmp" | "gif" | "jxl"
type Counts = Partial<Record<Format, number>>
type Entry = { source: string; output: string | null; status: "success" | "failed" | "skipped"; message: string }
type Preview = {
  supported: number; nonImages: number; unsupported: number; unreadable: number; ignored: number
  conflicts: number; scheduled: number; inputs: Counts; outputs: Counts; destination: string
  warnings: string[]; samples: Entry[]
}
type Report = {
  success: number; failed: number; skipped: number; durationMs: number
  cancelled: boolean; destination: string; entries: Entry[]
}
type Progress = { completed: number; total: number; file: string }
type Advanced = { workers: number; memoryMib: number; ignoreHidden: boolean; flattenAlpha: boolean; diagnostics: boolean }
type Settings = { theme: Theme; preview: boolean; recursive: boolean; structure: boolean; rememberOutput: boolean; lastOutput: string | null; advanced: Advanced }

const element = <T extends HTMLElement = HTMLElement>(id: string): T => {
  const value = document.getElementById(id)
  if (!value) throw new Error("Missing interface element " + id)
  return value as T
}
const select = (id: string) => element<HTMLSelectElement>(id)
const input = (id: string) => element<HTMLInputElement>(id)
const button = (id: string) => element<HTMLButtonElement>(id)
const pages: Page[] = ["home", "preview", "processing", "results", "settings", "advanced"]
const formatNames: Record<Format, string> = {
  jpeg: "JPEG", png: "PNG", webp: "WebP", heic: "HEIC/HEIF", avif: "AVIF", tiff: "TIFF", bmp: "BMP", gif: "GIF", jxl: "JPEG XL",
}
const advancedDefaults: Advanced = { workers: 0, memoryMib: 768, ignoreHidden: false, flattenAlpha: false, diagnostics: false }
let settings: Settings = { theme: "system", preview: true, recursive: true, structure: true, rememberOutput: true, lastOutput: null, advanced: { ...advancedDefaults } }
let page: Page = "home"
let source: string | null = null
let destinationFolder: string | null = null
let pending: { id: number; preview: Preview; destination: Destination } | null = null
let resultEntries: Entry[] = []
let visibleResults = 0
let preparing = false
let folderDialogOpen = false
let settingsSave: Promise<void> = Promise.resolve()

function readable(path: string): string {
  if (path.startsWith("\\\\?\\UNC\\")) return "\\\\" + path.slice(8)
  if (path.startsWith("\\\\?\\")) return path.slice(4)
  return path
}

function fileName(path: string): string {
  return readable(path).split(/[\\/]/).filter(Boolean).at(-1) ?? readable(path)
}

function notify(message: unknown, error = false) {
  const notification = element("notification")
  notification.hidden = false
  notification.setAttribute("role", error ? "alert" : "status")
  notification.textContent = message instanceof Error ? message.message : String(message)
}

function clearNotice() {
  element("notification").hidden = true
}

function show(next: Page) {
  page = next
  for (const name of pages) element(name).hidden = name !== next
  button("settings-button").hidden = next !== "home" && next !== "results"
  clearNotice()
  window.scrollTo({ top: 0 })
  element(next + "-title").focus({ preventScroll: true })
}

function updateHome() {
  element("source-label").textContent = source ? fileName(source) : "Drop a folder here"
  element("source-path").textContent = source ? readable(source) : "One folder per batch"
  button("source-button").textContent = source ? "Change folder" : "Choose folder"
  element("output-path").textContent = destinationFolder ? readable(destinationFolder) : "No output folder selected"
  const destination = select("destination").value
  element("output-row").hidden = destination !== "folder"
  element("overwrite-note").hidden = destination !== "overwrite"
  const format = select("format").value
  element("quality-row").hidden = !["jpeg", "avif"].includes(format)
  element("quality-value").textContent = input("quality").value
  const editing = select("metadata").value === "edit"
  element("editor").hidden = !editing
  for (const field of element("editor").querySelectorAll<HTMLInputElement | HTMLTextAreaElement>("input, textarea")) {
    field.disabled = !editing
  }
  element("scan-scope").textContent = settings.recursive ? "Subfolders included" : "Top-level folder only"
  button("prepare-button").disabled = preparing || !source || (destination === "folder" && !destinationFolder)
  const label = preparing ? "Scanning folder…" : settings.preview ? "Review batch" : "Start Processing"
  button("prepare-button").firstChild!.textContent = label + " "
}

function updateSettings() {
  if (settings.theme === "system") delete document.documentElement.dataset.theme
  else document.documentElement.dataset.theme = settings.theme
  select("theme").value = settings.theme
  input("auto-preview").checked = settings.preview
  input("recursive").checked = settings.recursive
  input("structure").checked = settings.structure
  input("remember-output").checked = settings.rememberOutput
  select("worker-mode").value = settings.advanced.workers ? "manual" : "automatic"
  element("workers-row").hidden = settings.advanced.workers === 0
  input("workers").value = String(settings.advanced.workers || 2)
  select("memory").value = String(settings.advanced.memoryMib)
  input("ignore-hidden").checked = settings.advanced.ignoreHidden
  input("flatten-alpha").checked = settings.advanced.flattenAlpha
  input("diagnostics").checked = settings.advanced.diagnostics
  updateHome()
}

function saveSettings() {
  const snapshot = structuredClone(settings)
  settingsSave = settingsSave
    .then(() => invoke<void>("save_settings", { settings: snapshot }))
    .catch(error => notify("Settings could not be saved. " + String(error), true))
}

async function chooseFolder(output: boolean) {
  if (preparing || folderDialogOpen) return
  folderDialogOpen = true
  try {
    const path = await invoke<string | null>("pick_folder", { output })
    if (!path) return
    clearNotice()
    if (output) {
      destinationFolder = path
      settings.lastOutput = settings.rememberOutput ? path : null
      saveSettings()
    } else {
      source = path
    }
    updateHome()
  } catch (error) {
    notify(error, true)
  } finally {
    folderDialogOpen = false
  }
}

function collectRequest() {
  const editIds = ["captured", "author", "copyright", "title", "description", "latitude", "longitude", "make", "model", "lens"]
  return {
    source,
    output: destinationFolder,
    format: select("format").value || null,
    quality: input("quality").valueAsNumber,
    metadata: select("metadata").value,
    edits: Object.fromEntries(editIds.map(id => [id, element<HTMLInputElement | HTMLTextAreaElement>(id).value.trim()])),
    destination: select("destination").value as Destination,
    conflict: select("conflict").value,
    confirmedOverwrite: false,
  }
}

function metrics(container: string, rows: [string, number][]) {
  element(container).replaceChildren(...rows.map(([label, count]) => {
    const card = document.createElement("div")
    const value = document.createElement("strong")
    const caption = document.createElement("span")
    card.className = "card metric"
    value.textContent = count.toLocaleString()
    caption.textContent = label
    card.append(value, caption)
    return card
  }))
}

function recordRow(record: Entry): HTMLLIElement {
  const item = document.createElement("li")
  const name = document.createElement("strong")
  const detail = document.createElement("span")
  const status = record.status === "success" ? "Saved" : record.status === "failed" ? "Failed" : "Skipped"
  name.textContent = status + " · " + readable(record.source)
  detail.textContent = record.message
  item.append(name, detail)
  return item
}

function showPreview(preview: Preview, metadataLabel: string) {
  metrics("preview-metrics", [
    ["Supported images", preview.supported], ["Non-image files", preview.nonImages],
    ["Unsupported images", preview.unsupported], ["Unreadable entries", preview.unreadable],
    ["Filename conflicts", preview.conflicts],
  ])
  const formats = (counts: Counts) => Object.entries(counts)
    .map(([format, count]) => formatNames[format as Format] + " (" + count + ")").join(", ") || "None"
  const rows = [
    ["Input formats", formats(preview.inputs)], ["Planned outputs", formats(preview.outputs)],
    ["Metadata", metadataLabel], ["Destination", readable(preview.destination)],
    ["Images to process", String(preview.scheduled)],
  ]
  if (preview.ignored) rows.push(["Ignored entries", String(preview.ignored)])
  element("preview-details").replaceChildren(...rows.flatMap(([label, value]) => {
    const title = document.createElement("dt")
    const description = document.createElement("dd")
    title.textContent = label
    description.textContent = value
    return [title, description]
  }))
  element("preview-notes").hidden = preview.warnings.length === 0
  element("preview-warnings").replaceChildren(...preview.warnings.map(warning => {
    const item = document.createElement("li")
    item.textContent = warning
    return item
  }))
  element("preview-issues").hidden = preview.samples.length === 0
  element("preview-files").replaceChildren(...preview.samples.map(recordRow))
  show("preview")
  if (!preview.scheduled) notify("No images are scheduled. Start Processing to view or export the scan results.")
}

function confirmOverwrite(): Promise<boolean> {
  const dialog = element<HTMLDialogElement>("overwrite-dialog")
  dialog.returnValue = "cancel"
  dialog.showModal()
  return new Promise(resolve => {
    dialog.addEventListener("close", () => resolve(dialog.returnValue === "confirm"), { once: true })
  })
}

function appendResults() {
  const next = resultEntries.slice(visibleResults, visibleResults + 50)
  element("result-files").append(...next.map(recordRow))
  visibleResults += next.length
  button("more-results").hidden = visibleResults >= resultEntries.length
}

function showReport(report: Report) {
  metrics("result-metrics", [["Successful", report.success], ["Failed", report.failed], ["Skipped", report.skipped]])
  element("results-title").textContent = report.cancelled ? "Batch cancelled" : "Batch complete"
  const seconds = report.durationMs / 1000
  element("duration").textContent = seconds < 1 ? "Finished in " + report.durationMs + " ms" : "Finished in " + seconds.toFixed(1) + " s"
  element("result-path").textContent = readable(report.destination)
  resultEntries = report.entries.filter(record => record.message).sort((a, b) => {
    const order = { failed: 0, skipped: 1, success: 2 }
    return order[a.status] - order[b.status]
  })
  element("result-files").replaceChildren()
  visibleResults = 0
  appendResults()
  element("result-notes").hidden = !resultEntries.length
  show("results")
}

async function start() {
  if (!pending || page === "processing") return
  const batch = pending
  const confirmation = batch.destination !== "overwrite" || batch.preview.scheduled === 0 || await confirmOverwrite()
  if (!confirmation) return
  pending = null
  const progress = element<HTMLProgressElement>("progress")
  progress.max = Math.max(batch.preview.scheduled, 1)
  progress.value = 0
  element("progress-count").textContent = "Preparing workers…"
  element("progress-file").textContent = ""
  button("cancel-button").disabled = false
  button("cancel-button").textContent = "Cancel processing"
  show("processing")
  const channel = new Channel<Progress>()
  channel.onmessage = event => {
    progress.max = Math.max(event.total, 1)
    progress.value = event.completed
    element("progress-count").textContent = event.completed + " of " + event.total + " images processed"
    element("progress-file").textContent = readable(event.file)
  }
  try {
    const report = await invoke<Report>("process", { id: batch.id, confirmed: confirmation, onProgress: channel })
    showReport(report)
  } catch (error) {
    show("home")
    notify(error, true)
  }
}

async function prepare(event: SubmitEvent) {
  event.preventDefault()
  if (preparing || !source) return
  clearNotice()
  preparing = true
  updateHome()
  const request = collectRequest()
  const label = select("metadata").selectedOptions[0].textContent ?? ""
  button("settings-button").disabled = true
  try {
    const batch = await invoke<{ id: number; preview: Preview }>("analyze", { request, settings })
    pending = { ...batch, destination: request.destination }
    if (settings.preview) showPreview(batch.preview, label)
    else await start()
  } catch (error) {
    notify(error, true)
  } finally {
    preparing = false
    button("settings-button").disabled = false
    updateHome()
  }
}

async function cancel() {
  button("cancel-button").disabled = true
  button("cancel-button").textContent = "Cancelling…"
  try {
    await invoke("cancel_batch")
  } catch (error) {
    notify(error, true)
    button("cancel-button").disabled = false
    button("cancel-button").textContent = "Cancel processing"
  }
}

async function exportReport(csv: boolean) {
  try {
    const path = await invoke<string | null>("export_report", { csv })
    if (path) notify("Report saved to " + readable(path))
  } catch (error) {
    notify(error, true)
  }
}

function bindSettings(id: string, change: () => void) {
  element(id).addEventListener("change", () => {
    change()
    updateSettings()
    saveSettings()
  })
}

button("settings-button").addEventListener("click", () => show("settings"))
document.querySelectorAll<HTMLButtonElement>("button[data-page]").forEach(control => {
  control.addEventListener("click", () => show(control.dataset.page as Page))
})
button("source-button").addEventListener("click", () => void chooseFolder(false))
button("output-button").addEventListener("click", () => void chooseFolder(true))
button("preview-start").addEventListener("click", () => void start())
button("cancel-button").addEventListener("click", () => void cancel())
button("more-results").addEventListener("click", appendResults)
button("open-output").addEventListener("click", () => void invoke("open_output").catch(error => notify(error, true)))
button("export-csv").addEventListener("click", () => void exportReport(true))
button("export-txt").addEventListener("click", () => void exportReport(false))
element<HTMLFormElement>("batch-form").addEventListener("submit", event => void prepare(event))
for (const id of ["format", "metadata", "destination", "quality"]) {
  element(id).addEventListener("input", updateHome)
}
bindSettings("theme", () => { settings.theme = select("theme").value as Theme })
bindSettings("auto-preview", () => { settings.preview = input("auto-preview").checked })
bindSettings("recursive", () => { settings.recursive = input("recursive").checked })
bindSettings("structure", () => { settings.structure = input("structure").checked })
bindSettings("remember-output", () => {
  settings.rememberOutput = input("remember-output").checked
  settings.lastOutput = settings.rememberOutput ? destinationFolder : null
})
bindSettings("worker-mode", () => { settings.advanced.workers = select("worker-mode").value === "manual" ? Number(input("workers").value) : 0 })
bindSettings("workers", () => { settings.advanced.workers = Math.min(16, Math.max(1, Number(input("workers").value) || 1)) })
bindSettings("memory", () => { settings.advanced.memoryMib = Number(select("memory").value) })
bindSettings("ignore-hidden", () => { settings.advanced.ignoreHidden = input("ignore-hidden").checked })
bindSettings("flatten-alpha", () => { settings.advanced.flattenAlpha = input("flatten-alpha").checked })
bindSettings("diagnostics", () => { settings.advanced.diagnostics = input("diagnostics").checked })
button("reset-settings").addEventListener("click", () => {
  settings.advanced = { ...advancedDefaults }
  updateSettings()
  saveSettings()
  notify("Advanced settings reset.")
})

async function initialize() {
  updateSettings()
  void getVersion().then(version => {
    element("about-build").textContent = "v" + version + " · Tauri 2 and Rust"
  }).catch(error => console.error("Could not read the Pixhift version", error))
  try {
    settings = await invoke<Settings>("load_settings")
    if (settings.rememberOutput) destinationFolder = settings.lastOutput
    updateSettings()
  } catch (error) {
    notify("Using default settings. " + String(error), true)
  }
  try {
    await getCurrentWebview().onDragDropEvent(async ({ payload }) => {
      const zone = element("drop-zone")
      if (payload.type === "leave" || payload.type === "drop") delete zone.dataset.dragging
      if (page !== "home" || preparing) return
      if (payload.type === "enter" || payload.type === "over") zone.dataset.dragging = "true"
      if (payload.type === "drop") {
        try {
          source = await invoke<string>("dropped_folder", { paths: payload.paths })
          clearNotice()
          updateHome()
        } catch (error) {
          notify(error, true)
        }
      }
    })
  } catch (error) {
    notify("Folder drag-and-drop is unavailable. Use Choose folder. " + String(error), true)
  }
}

void initialize()
