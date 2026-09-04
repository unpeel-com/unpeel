#!/usr/bin/env bun
// Generate client-safe runtime metadata from each runtime.toml under runtimes/.
//
// The Rust Host consumes the descriptors directly. Swift cannot import that
// catalog without adding a runtime bridge, so it checks in one deterministic
// generated compatibility catalog. Only declared metadata/capabilities enter
// this output; provider recipes remain Host-owned.

import { lstat, readdir, realpath } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptPath = fileURLToPath(import.meta.url)
const repoRoot = path.dirname(path.dirname(scriptPath))
const runtimesRoot = path.join(repoRoot, 'runtimes')
// The generated Swift file is an OUTPUT the Apple client repo regenerates
// from this repo's runtimes/ at its pinned server version
// (`bun scripts/generate-runtime-client-catalog.mjs --out <path>` from a
// checkout of the pinned tag). The default writes `generated/GeneratedRuntimeCatalog.swift`
// here (checked in as the reference copy the Apple repo copies); `--check`
// verifies whichever path.
const cliArgs = process.argv.slice(2)
const outIndex = cliArgs.indexOf('--out')
const swiftOutput = outIndex >= 0 && cliArgs[outIndex + 1]
  ? path.resolve(cliArgs[outIndex + 1])
  : path.join(
      repoRoot,
      'generated/GeneratedRuntimeCatalog.swift'
    )
const colorPattern = /^#[0-9A-Fa-f]{6}$/
const maxRuntimeIconBytes = 128 * 1024

function swiftString(value) {
  return JSON.stringify(value)
}

function swiftOptionalString(value) {
  return value == null ? 'nil' : swiftString(value)
}

function swiftColor(value, source, field) {
  if (value == null) return 'nil'
  if (!colorPattern.test(value)) {
    throw new Error(`${source}: ${field} must be #RRGGBB, got ${JSON.stringify(value)}`)
  }
  return `0x${value.slice(1).toUpperCase()}`
}

function swiftStringArray(values) {
  return `[${values.map(swiftString).join(', ')}]`
}

const runtimeKinds = {
  agent: '.agent',
  app: '.app',
  editor: '.editor',
  terminal: '.terminal',
}

function swiftRuntimeKind(value, source) {
  const kind = value ?? 'agent'
  const swift = runtimeKinds[kind]
  if (!swift) {
    throw new Error(
      `${path.relative(repoRoot, source)}: display.kind must be agent, app, editor, or terminal`
    )
  }
  return swift
}

const maxWindowPaddingX = 48

function swiftWindowPaddingX(display, source) {
  const value = display.window_padding_x ?? 0
  if (
    typeof value !== 'number' ||
    !Number.isInteger(value) ||
    value < 0 ||
    value > maxWindowPaddingX
  ) {
    throw new Error(
      `${path.relative(repoRoot, source)}: display.window_padding_x must be an integer 0–${maxWindowPaddingX}`
    )
  }
  return String(value)
}

async function loadIconSVG(display, source) {
  const relativePath = display.icon_asset
  const iconSource = display.icon_source
  const iconLicense = display.icon_license
  if (relativePath == null) {
    if (iconSource != null || iconLicense != null) {
      throw new Error(
        `${path.relative(repoRoot, source)}: icon_source/icon_license require display.icon_asset`
      )
    }
    if (display.icon_template === false) {
      throw new Error(
        `${path.relative(repoRoot, source)}: display.icon_template=false requires icon_asset`
      )
    }
    return null
  }

  if (typeof relativePath !== 'string') {
    throw new Error(`${path.relative(repoRoot, source)}: display.icon_asset must be a string`)
  }
  const segments = relativePath.split('/')
  if (
    path.isAbsolute(relativePath) ||
    relativePath.includes('\\') ||
    segments[0] !== 'assets' ||
    segments.some((segment) => segment === '' || segment === '.' || segment === '..') ||
    path.posix.extname(relativePath) !== '.svg'
  ) {
    throw new Error(
      `${path.relative(repoRoot, source)}: display.icon_asset must be a safe SVG path below assets/`
    )
  }
  if (
    typeof iconSource !== 'string' ||
    !(iconSource.startsWith('https://') || iconSource.startsWith('internal:')) ||
    /\s/.test(iconSource) ||
    typeof iconLicense !== 'string' ||
    iconLicense.trim() === '' ||
    iconLicense.trim() !== iconLicense
  ) {
    throw new Error(
      `${path.relative(repoRoot, source)}: icon assets require valid display.icon_source and display.icon_license`
    )
  }

  const assetPath = path.join(path.dirname(source), relativePath)
  const asset = Bun.file(assetPath)
  if (!(await asset.exists())) {
    throw new Error(
      `${path.relative(repoRoot, source)}: missing ${path.relative(repoRoot, assetPath)}`
    )
  }
  const assetMetadata = await lstat(assetPath)
  if (assetMetadata.isSymbolicLink() || !assetMetadata.isFile()) {
    throw new Error(`${path.relative(repoRoot, assetPath)} must be a regular file, not a symlink`)
  }
  const [runtimeRoot, resolvedAssetPath] = await Promise.all([
    realpath(path.dirname(source)),
    realpath(assetPath)
  ])
  const resolvedRelativePath = path.relative(runtimeRoot, resolvedAssetPath)
  if (
    path.isAbsolute(resolvedRelativePath) ||
    resolvedRelativePath === '..' ||
    resolvedRelativePath.startsWith(`..${path.sep}`)
  ) {
    throw new Error(`${path.relative(repoRoot, assetPath)} escapes its runtime package`)
  }
  if (assetMetadata.size > maxRuntimeIconBytes) {
    throw new Error(
      `${path.relative(repoRoot, assetPath)} exceeds the ${maxRuntimeIconBytes} byte icon limit`
    )
  }
  const svg = (await asset.text()).trim()
  if (svg === '' || !svg.includes('<svg') || !svg.includes('</svg>')) {
    throw new Error(`${path.relative(repoRoot, assetPath)} is not a complete UTF-8 SVG document`)
  }
  return svg
}

const capabilityCases = new Map([
  ['lifecycle_hooks', 'lifecycleHooks'],
  ['resume', 'resume'],
  ['restart_agent', 'restartAgent'],
  ['mcp_sessions', 'mcpSessions'],
  ['mcp_browser', 'mcpBrowser'],
  ['mcp_computer', 'mcpComputer'],
  ['transcript', 'transcript'],
  ['notify_when_done', 'notifyWhenDone'],
  ['semantic_terminal_title', 'semanticTerminalTitle']
])

const platformCases = new Map([
  ['macos', 'macos'],
  ['linux', 'linux']
])

function swiftPlatforms(values, source) {
  return `Set([${values.map((value) => {
    const swiftCase = platformCases.get(value)
    if (!swiftCase) {
      throw new Error(`${source}: unknown platform ${JSON.stringify(value)}`)
    }
    return `.${swiftCase}`
  }).join(', ')}])`
}

function swiftCapabilities(values, source) {
  return `[${values.map((value) => {
    const swiftCase = capabilityCases.get(value)
    if (!swiftCase) {
      throw new Error(`${source}: unknown capability ${JSON.stringify(value)}`)
    }
    return `.${swiftCase}`
  }).join(', ')}]`
}

async function loadDescriptors() {
  const directoryEntries = await readdir(runtimesRoot, { withFileTypes: true })
  const descriptors = []
  for (const entry of directoryEntries) {
    if (!entry.isDirectory()) continue
    const source = path.join(runtimesRoot, entry.name, 'runtime.toml')
    const file = Bun.file(source)
    if (!(await file.exists())) continue
    let descriptor
    try {
      descriptor = Bun.TOML.parse(await file.text())
    } catch (error) {
      throw new Error(`${path.relative(repoRoot, source)}: ${error.message}`)
    }
    if (descriptor.slug !== entry.name) {
      throw new Error(
        `${path.relative(repoRoot, source)}: slug ${JSON.stringify(descriptor.slug)} ` +
          `must match directory ${JSON.stringify(entry.name)}`
      )
    }
    const iconSVG = await loadIconSVG(descriptor.display, source)
    descriptors.push({ source, descriptor, iconSVG })
  }
  if (descriptors.length === 0) {
    throw new Error(`${path.relative(repoRoot, runtimesRoot)}: no runtime.toml descriptors found`)
  }
  descriptors.sort((left, right) => {
    const leftOrder = left.descriptor.legacy_order ?? Number.MAX_SAFE_INTEGER
    const rightOrder = right.descriptor.legacy_order ?? Number.MAX_SAFE_INTEGER
    return (
      leftOrder - rightOrder ||
      left.descriptor.slug.localeCompare(right.descriptor.slug) ||
      left.descriptor.id.localeCompare(right.descriptor.id)
    )
  })
  return descriptors
}

function renderSwift(descriptors) {
  const lines = [
    '// Generated by scripts/generate-runtime-client-catalog.mjs.',
    '// Source of truth: runtimes/*/runtime.toml and declared icon assets. Do not edit by hand.',
    '',
    'import Foundation',
    '',
    'public enum UnpeelRuntimeCapability: String, Equatable, Hashable, Sendable {',
    ...Array.from(capabilityCases, ([rawValue, swiftCase]) =>
      `    case ${swiftCase} = ${swiftString(rawValue)}`
    ),
    '}',
    '',
    'public enum UnpeelRuntimePlatform: String, Equatable, Hashable, Sendable {',
    ...Array.from(platformCases, ([rawValue, swiftCase]) =>
      `    case ${swiftCase} = ${swiftString(rawValue)}`
    ),
    '}',
    '',
    '/// Presentation family for sidebar logos and generic fallbacks. Adding a',
    '/// runtime never requires a Swift case; set `display.kind` on the package',
    '/// (a markdown-editor CLI uses `editor`). New families are added here',
    '/// and generated — clients must not branch on command names.',
    'public enum UnpeelRuntimeKind: String, Equatable, Hashable, Sendable {',
    '    case agent = "agent"',
    '    case app = "app"',
    '    case editor = "editor"',
    '    case terminal = "terminal"',
    '}',
    '',
    'public struct UnpeelRuntimeSuggestedPreset: Equatable, Sendable {',
    '    public let id: String',
    '    public let label: String',
    '    public let command: String',
    '    public let quickLaunch: Bool',
    '}',
    '',
    'public struct UnpeelRuntimeUsageStore: Equatable, Sendable {',
    '    public let root: String',
    '    public let extensions: Set<String>',
    '    public let fileName: String?',
    '    public let fileNameSuffix: String?',
    '    public let parentDirName: String?',
    '}',
    '',
    'public struct UnpeelRuntimeMetadata: Equatable, Sendable {',
    '    public let stableID: String',
    '    public let slug: String',
    '    public let legacySlug: String',
    '    public let legacyOrder: Int?',
    '    public let label: String',
    '    public let platforms: Set<UnpeelRuntimePlatform>',
    '    public let supportsQuickLaunch: Bool',
    '    public let kind: UnpeelRuntimeKind',
    '    public let tintColorHex: Int?',
    '    public let spinnerTintColorHex: Int?',
    '    public let iconKey: String',
    '    public let iconSVG: String?',
    '    public let iconIsTemplate: Bool',
    '    public let iconSource: String?',
    '    public let iconLicense: String?',
    '    /// Horizontal Ghostty window padding in points. Zero is edge-to-edge.',
    '    public let windowPaddingX: Int',
    '    public let installURL: String?',
    '    public let installCommand: String?',
    '    public let commandAliases: [String]',
    '    public let processAliases: [String]',
    '    public let searchPathSuffixes: [String]',
    '    public let lifecycleSource: String',
    '    public let lifecycleAuthority: String',
    '    public let lifecycleFallback: String',
    '    public let completionReliable: Bool',
    '    public let attentionReliable: Bool',
    '    public let anchorStartEventToOutput: Bool',
    '    public let attentionClearsOnOutput: Bool',
    '    public let distrustStopsWhileOutputGrows: Bool',
    '    public let capabilities: Set<UnpeelRuntimeCapability>',
    '    public let usageStores: [UnpeelRuntimeUsageStore]',
    '    public let suggestedPresets: [UnpeelRuntimeSuggestedPreset]',
    '',
    '    public func supports(_ platform: UnpeelRuntimePlatform) -> Bool {',
    '        platforms.contains(platform)',
    '    }',
    '    public var defaultPreset: UnpeelRuntimeSuggestedPreset? { suggestedPresets.first }',
    '    public var presentationCommand: String? { commandAliases.first }',
    '}',
    '',
    '/// Presentation, setup, and declared-capability metadata shared by clients.',
    '/// Runtime recipes and effective per-Session actions remain Host-owned.',
    'public enum UnpeelRuntimeCatalog {',
    '    public static let runtimes: [UnpeelRuntimeMetadata] = [',
  ]

  for (const { source, descriptor, iconSVG } of descriptors) {
    const display = descriptor.display
    const install = descriptor.install
    const detection = descriptor.detection
    const lifecycle = descriptor.lifecycle
    const usageStores = descriptor.usage?.stores ?? []
    const presets = descriptor.suggested_presets ?? []
    lines.push(
      `        // ${path.relative(repoRoot, source)}`,
      '        UnpeelRuntimeMetadata(',
      `            stableID: ${swiftString(descriptor.id)},`,
      `            slug: ${swiftString(descriptor.slug)},`,
      `            legacySlug: ${swiftString(descriptor.legacy_slug)},`,
      `            legacyOrder: ${descriptor.legacy_order ?? 'nil'},`,
      `            label: ${swiftString(descriptor.label)},`,
      `            platforms: ${swiftPlatforms(descriptor.platforms, source)},`,
      `            supportsQuickLaunch: ${Boolean(descriptor.supports_quick_launch)},`,
      `            kind: ${swiftRuntimeKind(display.kind, source)},`,
      `            tintColorHex: ${swiftColor(display.tint, source, 'display.tint')},`,
      `            spinnerTintColorHex: ${swiftColor(display.spinner_tint, source, 'display.spinner_tint')},`,
      `            iconKey: ${swiftString(display.icon ?? 'agent')},`,
      `            iconSVG: ${swiftOptionalString(iconSVG)},`,
      `            iconIsTemplate: ${Boolean(display.icon_template ?? true)},`,
      `            iconSource: ${swiftOptionalString(display.icon_source)},`,
      `            iconLicense: ${swiftOptionalString(display.icon_license)},`,
      `            windowPaddingX: ${swiftWindowPaddingX(display, source)},`,
      `            installURL: ${swiftOptionalString(install?.official_url)},`,
      `            installCommand: ${swiftOptionalString(install?.command)},`,
      `            commandAliases: ${swiftStringArray(detection.command_aliases)},`,
      `            processAliases: ${swiftStringArray(detection.process_aliases)},`,
      `            searchPathSuffixes: ${swiftStringArray(detection.search_path_suffixes ?? [])},`,
      `            lifecycleSource: ${swiftString(lifecycle.source)},`,
      `            lifecycleAuthority: ${swiftString(lifecycle.authority)},`,
      `            lifecycleFallback: ${swiftString(lifecycle.fallback)},`,
      `            completionReliable: ${Boolean(lifecycle.completion_reliable)},`,
      `            attentionReliable: ${Boolean(lifecycle.attention_reliable)},`,
      `            anchorStartEventToOutput: ${Boolean(lifecycle.anchor_start_event_to_output ?? true)},`,
      `            attentionClearsOnOutput: ${Boolean(lifecycle.attention_clears_on_output ?? true)},`,
      `            distrustStopsWhileOutputGrows: ${Boolean(lifecycle.distrust_stops_while_output_grows ?? false)},`,
      `            capabilities: ${swiftCapabilities(descriptor.capabilities ?? [], source)},`,
      '            usageStores: ['
    )
    for (const store of usageStores) {
      lines.push(
        '                UnpeelRuntimeUsageStore(',
        `                    root: ${swiftString(store.root)},`,
        `                    extensions: Set(${swiftStringArray(store.extensions)}),`,
        `                    fileName: ${swiftOptionalString(store.file_name)},`,
        `                    fileNameSuffix: ${swiftOptionalString(store.file_name_suffix)},`,
        `                    parentDirName: ${swiftOptionalString(store.parent_dir_name)}`,
        '                ),'
      )
    }
    lines.push(
      '            ],',
      '            suggestedPresets: ['
    )
    for (const preset of presets) {
      lines.push(
        '                UnpeelRuntimeSuggestedPreset(',
        `                    id: ${swiftString(preset.id)},`,
        `                    label: ${swiftString(preset.label)},`,
        `                    command: ${swiftString(preset.command)},`,
        `                    quickLaunch: ${Boolean(preset.quick_launch)}`,
        '                ),'
      )
    }
    lines.push('            ]', '        ),')
  }

  lines.push(
    '    ]',
    '',
    '    private static let identityIndex: [String: Int] = {',
    '        var result: [String: Int] = [:]',
    '        for (index, runtime) in runtimes.enumerated() {',
    '            let identities = [runtime.stableID, runtime.slug, runtime.legacySlug]',
    '                + runtime.processAliases',
    '            for identity in identities { result[identity.lowercased()] = index }',
    '        }',
    '        return result',
    '    }()',
    '',
    '    private static let commandIndex: [String: Int] = {',
    '        var result: [String: Int] = [:]',
    '        for (index, runtime) in runtimes.enumerated() {',
    '            for alias in runtime.commandAliases { result[alias.lowercased()] = index }',
    '        }',
    '        return result',
    '    }()',
    '',
    '    public static func runtime(id: String?) -> UnpeelRuntimeMetadata? {',
    '        guard let key = id?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),',
    '              let index = identityIndex[key] else { return nil }',
    '        return runtimes[index]',
    '    }',
    '',
    '    public static func runtime(command: String) -> UnpeelRuntimeMetadata? {',
    '        let token = command',
    '            .trimmingCharacters(in: .whitespacesAndNewlines)',
    '            .split(whereSeparator: { $0.isWhitespace })',
    '            .first',
    '            .map(String.init)',
    '        guard let token else { return nil }',
    '        let head = (token as NSString).lastPathComponent.lowercased()',
    '        guard let index = commandIndex[head] else { return nil }',
    '        return runtimes[index]',
    '    }',
    '',
    '    public static func runtimes(for platform: UnpeelRuntimePlatform) -> [UnpeelRuntimeMetadata] {',
    '        runtimes(for: platform, from: runtimes)',
    '    }',
    '',
    '    static func runtimes(',
    '        for platform: UnpeelRuntimePlatform,',
    '        from candidates: [UnpeelRuntimeMetadata]',
    '    ) -> [UnpeelRuntimeMetadata] {',
    '        candidates.filter { $0.supports(platform) }',
    '    }',
    '',
    '    public static func runtime(id: String?, for platform: UnpeelRuntimePlatform) -> UnpeelRuntimeMetadata? {',
    '        guard let runtime = runtime(id: id), runtime.supports(platform) else { return nil }',
    '        return runtime',
    '    }',
    '',
    '    public static func runtime(command: String, for platform: UnpeelRuntimePlatform) -> UnpeelRuntimeMetadata? {',
    '        guard let runtime = runtime(command: command), runtime.supports(platform) else { return nil }',
    '        return runtime',
    '    }',
    '',
    '    public static func presentationCommand(forRuntimeID id: String?) -> String? {',
    '        runtime(id: id)?.presentationCommand',
    '    }',
    '}',
    ''
  )
  return lines.join('\n')
}

async function main() {
  const check = cliArgs.includes('--check')
  const rendered = renderSwift(await loadDescriptors())
  const output = Bun.file(swiftOutput)
  if (check) {
    const current = (await output.exists()) ? await output.text() : ''
    if (current !== rendered) {
      console.error(
        `${path.relative(repoRoot, swiftOutput)} is stale; ` +
          'run bun scripts/generate-runtime-client-catalog.mjs'
      )
      process.exitCode = 1
    }
    return
  }
  await Bun.write(swiftOutput, rendered)
}

await main().catch((error) => {
  console.error(`runtime client catalog generation failed: ${error.message}`)
  process.exitCode = 1
})
