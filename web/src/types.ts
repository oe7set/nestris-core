// Schema v4 OutputFrame (the engine's wire contract).

/** Extended dashboard statistics (mirrors the engine's `ExtendedStats`). */
export interface ExtendedStats {
  points: {
    drops: number;
    singles: number;
    doubles: number;
    triples: number;
    tetrises: number;
  };
  efficiency: number | null;
  pace_score: number | null;
  i_drought: { current: number; last: number; max: number; count: number };
  board: {
    max_height: number;
    avg_height: number;
    holes: number;
    tetris_ready: boolean;
    double_well: boolean;
    clean_slope: boolean;
  };
  /** (total lines, tetris rate) sampled after each clear. */
  trt_trend: [number, number][];
  /** (ts seconds, max stack height, flag bits) sampled ~4 Hz. */
  height_timeline: [number, number, number][];
  piece_dist: { counts: number[]; drought: number[]; deviation: number };
}

export interface OutputFrame {
  schema_version: number;
  seq: number;
  ts: number;
  region: string;
  game_state: string;
  fields: {
    score: number | null;
    lines: number | null;
    level: number | null;
    next_piece: string | null;
    current_piece: string | null;
    current_piece_pos: [number, number] | null;
    current_piece_cells: [number, number][] | null;
    playfield: number[][] | null;
    statistics: Record<string, number | null> | null;
  };
  stats: {
    pps: number | null;
    tetris_rate: number | null;
    burn: number;
    drought: number;
    max_drought: number;
    clears: { single: number; double: number; triple: number; tetris: number };
    score_per_min: number | null;
    pieces: number;
    active_seconds: number | null;
  };
  stats_ext?: ExtendedStats | null;
  confidence: Record<string, number>;
  events: {
    ts: number;
    field: string;
    reason: string;
    severity: string;
    old: unknown;
    new: unknown;
    confidence: number | null;
  }[];
}
