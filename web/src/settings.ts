// Engine settings: a schema-driven form over the EngineConfig JSON
// (field names/defaults mirror crates/nestris-engine/src/config.rs; the wasm
// Engine constructor deserializes this JSON directly).

const STORAGE_KEY = "nestris-config";

export type EngineConfigJson = Record<string, unknown>;

export function defaultConfig(): EngineConfigJson {
  return {
    calibration: {
      revalidate_every_n: 3,
      acquire_threshold: 0.55,
      drift_threshold: 0.4,
      lost_frames: 12,
      acquire_frames: 2,
      smooth_alpha: 0.5,
      undistort: "auto",
      adopt_margin: 0.05,
      background_recalibration: true,
      menu_drift_hold: true,
      // Web defaults: acquisition runs in the recalib worker with downscaled
      // candidate detection so the preview stays smooth while searching.
      background_acquisition: true,
      acquire_downscale_width: 640,
    },
    fusion: {
      vote_window: 5,
      confidence_decay: 0.9,
      min_report_confidence: 0.4,
      enforce_monotonic: true,
      new_game_menu_frames: 10,
    },
    plausibility: {
      enabled: true,
      max_score_jump: 60000,
      max_lines_step: 4,
      max_lines_skip: 8,
      level_tolerance: 1,
      confirm_frames: 6,
    },
    recognition: {
      score_base: "auto",
      score_base_latch_frames: 60,
      read_statistics: true,
      statistics_every_n: 6,
      read_current_piece: true,
      freeze_on_clear_animation: true,
      playfield_stabilizer: true,
    },
    // Web defaults: continuous tracking on (handheld/camera sources are the
    // norm in a browser) and extended stats for the statistics section.
    tracking: {
      enabled: true,
      search_radius_px: 8,
      damping: 0.6,
    },
    output: {
      extended_stats: true,
    },
  };
}

interface FieldDef {
  path: string;
  label: string;
  kind: "number" | "bool" | "select";
  options?: string[];
  step?: number;
}

const SCHEMA: { group: string; fields: FieldDef[] }[] = [
  {
    group: "Calibration",
    fields: [
      { path: "calibration.acquire_threshold", label: "Acquire threshold", kind: "number", step: 0.05 },
      { path: "calibration.drift_threshold", label: "Drift threshold", kind: "number", step: 0.05 },
      { path: "calibration.lost_frames", label: "Lost after weak frames", kind: "number", step: 1 },
      { path: "calibration.acquire_frames", label: "Frames to confirm acquire", kind: "number", step: 1 },
      { path: "calibration.smooth_alpha", label: "Geometry smoothing (EMA)", kind: "number", step: 0.05 },
      { path: "calibration.adopt_margin", label: "Re-solve adopt margin", kind: "number", step: 0.01 },
      { path: "calibration.undistort", label: "Barrel undistortion", kind: "select", options: ["auto", "off"] },
      { path: "calibration.background_recalibration", label: "Background recalibration", kind: "bool" },
      { path: "calibration.background_acquisition", label: "Background acquisition", kind: "bool" },
      { path: "calibration.acquire_downscale_width", label: "Candidate detection width (0 = full res)", kind: "number", step: 40 },
      { path: "calibration.menu_drift_hold", label: "Hold lock through menus", kind: "bool" },
    ],
  },
  {
    group: "Fusion",
    fields: [
      { path: "fusion.vote_window", label: "Vote window (frames)", kind: "number", step: 1 },
      { path: "fusion.confidence_decay", label: "Confidence decay", kind: "number", step: 0.05 },
      { path: "fusion.min_report_confidence", label: "Min report confidence", kind: "number", step: 0.05 },
      { path: "fusion.enforce_monotonic", label: "Enforce monotonic values", kind: "bool" },
      { path: "fusion.new_game_menu_frames", label: "Menu frames to arm new game", kind: "number", step: 1 },
    ],
  },
  {
    group: "Plausibility",
    fields: [
      { path: "plausibility.enabled", label: "Enabled (NES-rules guard)", kind: "bool" },
      { path: "plausibility.max_score_jump", label: "Max score jump", kind: "number", step: 1000 },
      { path: "plausibility.max_lines_step", label: "Max lines step", kind: "number", step: 1 },
      { path: "plausibility.max_lines_skip", label: "Max lines skip", kind: "number", step: 1 },
      { path: "plausibility.level_tolerance", label: "Level tolerance", kind: "number", step: 1 },
      { path: "plausibility.confirm_frames", label: "Self-heal after frames", kind: "number", step: 1 },
    ],
  },
  {
    group: "Recognition",
    fields: [
      { path: "recognition.score_base", label: "Score base", kind: "select", options: ["auto", "dec", "hex"] },
      { path: "recognition.score_base_latch_frames", label: "Base latch frames", kind: "number", step: 10 },
      { path: "recognition.read_statistics", label: "Read STATISTICS rail", kind: "bool" },
      { path: "recognition.statistics_every_n", label: "STATISTICS every N frames", kind: "number", step: 1 },
      { path: "recognition.read_current_piece", label: "Track current piece", kind: "bool" },
      { path: "recognition.freeze_on_clear_animation", label: "Freeze during clear animation", kind: "bool" },
      { path: "recognition.playfield_stabilizer", label: "Playfield stabilizer", kind: "bool" },
    ],
  },
  {
    group: "Tracking",
    fields: [
      { path: "tracking.enabled", label: "Continuous tracking (handheld)", kind: "bool" },
      { path: "tracking.search_radius_px", label: "Label search radius (px)", kind: "number", step: 1 },
      { path: "tracking.damping", label: "Correction damping", kind: "number", step: 0.05 },
    ],
  },
  {
    group: "Output",
    fields: [
      { path: "output.extended_stats", label: "Extended statistics", kind: "bool" },
    ],
  },
];

function get(obj: EngineConfigJson, path: string): unknown {
  return path
    .split(".")
    .reduce<unknown>((o, k) => (o as Record<string, unknown>)?.[k], obj);
}

function set(obj: EngineConfigJson, path: string, value: unknown): void {
  const keys = path.split(".");
  let cursor = obj as Record<string, unknown>;
  for (const key of keys.slice(0, -1)) {
    cursor = (cursor[key] ??= {}) as Record<string, unknown>;
  }
  cursor[keys[keys.length - 1]] = value;
}

export function loadConfig(): EngineConfigJson {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      // Overlay saved values onto the defaults so new fields get defaults.
      const config = defaultConfig();
      const saved = JSON.parse(raw) as EngineConfigJson;
      for (const { fields } of SCHEMA) {
        for (const field of fields) {
          const value = get(saved, field.path);
          if (value !== undefined) set(config, field.path, value);
        }
      }
      return config;
    }
  } catch {
    /* fall through to defaults */
  }
  return defaultConfig();
}

export function saveConfig(config: EngineConfigJson): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(config));
}

/** Build the settings modal; calls `onApply(configJson)` when applied. */
export function initSettingsUi(onApply: (configJson: string) => void): void {
  const modal = document.getElementById("settings-modal")!;
  const form = document.getElementById("settings-form")!;
  let config = loadConfig();

  const render = (): void => {
    form.innerHTML = "";
    for (const { group, fields } of SCHEMA) {
      const heading = document.createElement("h3");
      heading.textContent = group;
      form.appendChild(heading);
      for (const field of fields) {
        const row = document.createElement("label");
        row.className = "settings-row";
        const caption = document.createElement("span");
        caption.textContent = field.label;
        row.appendChild(caption);
        const current = get(config, field.path);
        if (field.kind === "bool") {
          const input = document.createElement("input");
          input.type = "checkbox";
          input.checked = Boolean(current);
          input.onchange = () => set(config, field.path, input.checked);
          row.appendChild(input);
        } else if (field.kind === "select") {
          const select = document.createElement("select");
          for (const option of field.options ?? []) {
            const el = document.createElement("option");
            el.value = option;
            el.textContent = option;
            el.selected = current === option;
            select.appendChild(el);
          }
          select.onchange = () => set(config, field.path, select.value);
          row.appendChild(select);
        } else {
          const input = document.createElement("input");
          input.type = "number";
          input.step = String(field.step ?? 1);
          input.value = String(current ?? "");
          input.onchange = () => set(config, field.path, Number(input.value));
          row.appendChild(input);
        }
        form.appendChild(row);
      }
    }
  };

  document.getElementById("btn-settings")!.addEventListener("click", () => {
    config = loadConfig();
    render();
    modal.style.display = "flex";
  });
  document.getElementById("settings-close")!.addEventListener("click", () => {
    modal.style.display = "none";
  });
  document.getElementById("settings-reset")!.addEventListener("click", () => {
    config = defaultConfig();
    render();
  });
  document.getElementById("settings-apply")!.addEventListener("click", () => {
    saveConfig(config);
    modal.style.display = "none";
    onApply(JSON.stringify(config));
  });
  modal.addEventListener("click", (ev) => {
    if (ev.target === modal) modal.style.display = "none";
  });
}
