import { cp, mkdir, readdir, readFile, writeFile } from "node:fs/promises"

const api = "node_modules/@tauri-apps/api"
await mkdir("dist/tauri", { recursive: true })
const files = await readdir(api, { recursive: true, withFileTypes: true })
const assets = files
  .filter(file => file.isFile() && file.name.endsWith(".js"))
  .map(file => {
    const source = file.parentPath + "/" + file.name
    return cp(source, "dist/tauri/" + source.slice(api.length + 1).replace(/^[/\\]+/, ""))
  })
const compiled = await readFile("dist/main.js", "utf8")
await Promise.all([
  cp("src/index.html", "dist/index.html"),
  cp("src/style.css", "dist/style.css"),
  cp("assets/pixhift.svg", "dist/pixhift.svg"),
  writeFile("dist/main.js", compiled.replaceAll(/"@tauri-apps\/api\/([^"]+)"/g, '"./tauri/$1.js"')),
  ...assets,
])
