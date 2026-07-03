// Schema v4 OutputFrame (the engine's wire contract).

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
