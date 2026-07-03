"""Generate OpenCV/numpy golden test data for the Rust ``nestris-vision`` crate.

Run with the Python repo's environment (it imports ``nestris_ocr`` and needs
cv2/numpy/av)::

    cd D:\\Projekte\\Retroverse\\NestrisLTM_OCR
    uv run python ..\\nestris-core\\tools\\gen_cv_goldens.py

Writes input/expected pairs into ``nestris-core/testdata/cv/<domain>/`` with a
``cases.json`` manifest per domain. The pairs are small and committed; the Rust
golden tests assert equivalence at the tolerances documented in the port plan.

PNG channel-order rule (mirrored by the Rust test loader): every 3-channel
array is written with ``cv2.imwrite(path, arr[..., ::-1])`` so the PNG's RGB
channels hold the array's channels *verbatim* (channel 0 first). Grayscale is
written as-is. This makes PNGs lossless byte containers for BGR, Lab, and HSV
arrays alike without any implicit swap.

Determinism: one fixed numpy seed; cv2's global RNG is only exercised by the
RANSAC cases, whose call order in this script is fixed, so re-runs produce
identical goldens.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import av
import cv2
import numpy as np

# Resolve repos relative to this file: nestris-core/tools/gen_cv_goldens.py.
_CORE = Path(__file__).resolve().parents[1]
_PY_REPO = _CORE.parent / "NestrisLTM_OCR"
_OUT = _CORE / "testdata" / "cv"
sys.path.insert(0, str(_PY_REPO / "src"))

from nestris_ocr.geometry.calibration import Rectifier, estimate_geometry  # noqa: E402
from nestris_ocr.geometry.undistort import _build_map  # noqa: E402
from nestris_ocr.regions.layout import get_layout  # noqa: E402
from nestris_ocr.core.enums import Region  # noqa: E402

_RNG = np.random.RandomState(42)

_FIXTURES = _PY_REPO / "fixtures"
#: WIN clips to probe for the anamorphic warp golden (first that locks wins).
_WIN_CANDIDATES = sorted(_FIXTURES.glob("WIN_*.mp4"))


def _save(path: Path, arr: np.ndarray) -> None:
    """Write ``arr`` as a lossless PNG byte container (see module docstring)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    to_write = arr if arr.ndim == 2 else arr[..., ::-1]
    if not cv2.imwrite(str(path), to_write):
        raise RuntimeError(f"imwrite failed: {path}")


def _manifest(domain: str, cases: list[dict]) -> None:
    out = _OUT / domain / "cases.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(cases, indent=1), encoding="utf-8")
    print(f"{domain}: {len(cases)} cases")


def _decode_frame(video: Path, at_s: float) -> np.ndarray:
    """Decode the first frame at/after ``at_s`` as BGR."""
    container = av.open(str(video))
    try:
        stream = container.streams.video[0]
        if at_s > 0:
            container.seek(int(at_s * 1_000_000), backward=True)
        for frame in container.decode(stream):
            ts = frame.time if frame.time is not None else 0.0
            if ts >= at_s:
                return frame.to_ndarray(format="bgr24")
    finally:
        container.close()
    raise RuntimeError(f"no frame at {at_s}s in {video}")


def _gradient_bgr(w: int, h: int) -> np.ndarray:
    """A structured ramp covering channel extremes (0 and 255 included)."""
    x = np.linspace(0, 255, w, dtype=np.float64)
    y = np.linspace(0, 255, h, dtype=np.float64)
    b = np.tile(x, (h, 1))
    g = np.tile(y[:, None], (1, w))
    r = np.clip(255.0 - (b + g) / 2.0, 0, 255)
    return np.stack([b, g, r], axis=-1).round().astype(np.uint8)


# --------------------------------------------------------------------------
# Real reference material
# --------------------------------------------------------------------------


def _reference_frames() -> dict[str, np.ndarray]:
    """Real frames used across domains: clean emulator, CRT camera, WIN."""
    frames = {
        "tetris01": _decode_frame(_FIXTURES / "tetris_01.mp4", 60.0),
    }
    crt = _FIXTURES / "Tetris for NES (CRT Gameplay Footage) (720p_60fps_H264-128kbit_AAC).mp4"
    if crt.exists():
        frames["crt"] = _decode_frame(crt, 30.0)
    return frames


def _lock_geometry(image: np.ndarray):
    """Run the production geometry solver; returns the result (may be weak)."""
    return estimate_geometry(image, layout=get_layout(Region.NTSC), region=Region.NTSC)


# --------------------------------------------------------------------------
# Domains
# --------------------------------------------------------------------------


def gen_color(frames: dict[str, np.ndarray]) -> None:
    inputs: dict[str, np.ndarray] = {
        "noise": _RNG.randint(0, 256, (64, 64, 3), dtype=np.uint8),
        "gradient": _gradient_bgr(64, 64),
        "tetris01_crop": frames["tetris01"][100:196, 120:248],
    }
    if "crt" in frames:
        inputs["crt_crop"] = frames["crt"][300:396, 400:528]
    cases = []
    for name, bgr in inputs.items():
        _save(_OUT / "color" / f"{name}_in.png", bgr)
        _save(_OUT / "color" / f"{name}_gray.png", cv2.cvtColor(bgr, cv2.COLOR_BGR2GRAY))
        lab = cv2.cvtColor(bgr, cv2.COLOR_BGR2LAB)
        _save(_OUT / "color" / f"{name}_lab.png", lab)
        _save(_OUT / "color" / f"{name}_hsv.png", cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV))
        _save(_OUT / "color" / f"{name}_lab2bgr.png", cv2.cvtColor(lab, cv2.COLOR_LAB2BGR))
        cases.append({"name": name, "w": bgr.shape[1], "h": bgr.shape[0]})
    _manifest("color", cases)


def gen_otsu(frames: dict[str, np.ndarray], canon: np.ndarray | None) -> None:
    inputs: dict[str, np.ndarray] = {
        "noise": _RNG.randint(0, 256, (32, 48), dtype=np.uint8),
        "bimodal": np.where(
            _RNG.rand(40, 40) > 0.4,
            _RNG.randint(160, 220, (40, 40)),
            _RNG.randint(10, 60, (40, 40)),
        ).astype(np.uint8),
    }
    if canon is not None:
        layout = get_layout(Region.NTSC)
        nb = layout.next_box
        gray = cv2.cvtColor(canon, cv2.COLOR_BGR2GRAY)
        inputs["next_box"] = gray[nb.y : nb.y + nb.h, nb.x : nb.x + nb.w].copy()
    cases = []
    for name, gray in inputs.items():
        thresh, binary = cv2.threshold(gray, 0, 255, cv2.THRESH_BINARY + cv2.THRESH_OTSU)
        _save(_OUT / "otsu" / f"{name}_in.png", gray)
        _save(_OUT / "otsu" / f"{name}_bin.png", binary)
        cases.append({"name": name, "thresh": float(thresh)})
    _manifest("otsu", cases)


def gen_morph() -> None:
    k3 = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (3, 3))
    k5 = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (5, 5))
    kernels = {"ellipse_3": k3.tolist(), "ellipse_5": k5.tolist()}
    (_OUT / "morph").mkdir(parents=True, exist_ok=True)
    (_OUT / "morph" / "kernels.json").write_text(json.dumps(kernels), encoding="utf-8")

    # 0/1 masks (anchors.py convention) and 0/255 binaries (next_piece.py).
    blob01 = (_RNG.rand(48, 64) > 0.55).astype(np.uint8)
    blob255 = ((_RNG.rand(40, 40) > 0.5) * 255).astype(np.uint8)
    inputs = {"mask01": blob01, "bin255": blob255}
    # The exact op sequences the engine runs:
    ops = [
        ("close_k3_i2", lambda im: cv2.morphologyEx(im, cv2.MORPH_CLOSE, k3, iterations=2)),
        ("open_k3_i1", lambda im: cv2.morphologyEx(im, cv2.MORPH_OPEN, k3, iterations=1)),
        ("open_k5_i1", lambda im: cv2.morphologyEx(im, cv2.MORPH_OPEN, k5, iterations=1)),
        ("erode_k3", lambda im: cv2.erode(im, k3)),
        ("dilate_k3", lambda im: cv2.dilate(im, k3)),
    ]
    cases = []
    for name, im in inputs.items():
        _save(_OUT / "morph" / f"{name}_in.png", im)
        for op_name, fn in ops:
            _save(_OUT / "morph" / f"{name}_{op_name}.png", fn(im))
        cases.append({"name": name, "ops": [o for o, _ in ops]})
    _manifest("morph", cases)


def gen_ncc(canon: np.ndarray | None) -> None:
    cases = []

    def add_case(name: str, image: np.ndarray, templ: np.ndarray) -> None:
        resp = cv2.matchTemplate(image, templ, cv2.TM_CCOEFF_NORMED)
        _, max_val, _, max_loc = cv2.minMaxLoc(resp)
        _save(_OUT / "ncc" / f"{name}_image.png", image)
        _save(_OUT / "ncc" / f"{name}_templ.png", templ)
        cases.append(
            {
                "name": name,
                "response": [[float(v) for v in row] for row in resp],
                "max_val": float(max_val),
                "max_loc": [int(max_loc[0]), int(max_loc[1])],
            }
        )

    noise_img = _RNG.randint(0, 256, (24, 40), dtype=np.uint8)
    noise_tpl = noise_img[8:16, 12:20].copy()
    add_case("noise", noise_img, noise_tpl)

    templates = _PY_REPO / "assets" / "templates" / "common"
    score_tpl = cv2.imread(str(templates / "labels" / "SCORE.png"), cv2.IMREAD_GRAYSCALE)
    digit_tpl = cv2.imread(str(templates / "digits" / "5.png"), cv2.IMREAD_GRAYSCALE)
    if canon is not None and score_tpl is not None:
        layout = get_layout(Region.NTSC)
        gray = cv2.cvtColor(canon, cv2.COLOR_BGR2GRAY)
        lr = layout.label_score
        pad = 8
        y0, x0 = max(0, lr.y - pad), max(0, lr.x - pad)
        window = gray[y0 : lr.y + lr.h + pad, x0 : lr.x + lr.w + pad].copy()
        add_case("score_label", window, score_tpl)
    if canon is not None and digit_tpl is not None:
        gray = cv2.cvtColor(canon, cv2.COLOR_BGR2GRAY)
        sc = get_layout(Region.NTSC).score
        window = gray[sc.y - 2 : sc.y + sc.h + 2, sc.x - 2 : sc.x + 18].copy()
        add_case("digit", window, digit_tpl)
    _manifest("ncc", cases)


def gen_components() -> None:
    cases = []
    inputs = {
        "blobs": (_RNG.rand(48, 64) > 0.7).astype(np.uint8),
        "rects": np.zeros((60, 80), dtype=np.uint8),
    }
    inputs["rects"][5:25, 10:30] = 1
    inputs["rects"][30:55, 40:75] = 1
    inputs["rects"][2:6, 60:64] = 1  # touches the big rect diagonally? no: isolated
    for name, im in inputs.items():
        n, labels, stats, centroids = cv2.connectedComponentsWithStats(im, connectivity=8)
        comps = []
        for i in range(1, n):
            x, y, w, h, area = (int(v) for v in stats[i])
            cx, cy = (float(v) for v in centroids[i])
            comps.append({"area": area, "x": x, "y": y, "w": w, "h": h, "cx": cx, "cy": cy})
        # Sorted by (area desc, x, y): label IDs are OpenCV scan-order internals
        # the Rust implementation need not reproduce.
        comps.sort(key=lambda c: (-c["area"], c["x"], c["y"]))
        _save(_OUT / "cc" / f"{name}_in.png", im)
        cases.append({"name": name, "components": comps})
    _manifest("cc", cases)


def gen_min_area_rect() -> None:
    cases = []

    def add_case(name: str, pts: np.ndarray) -> None:
        rect = cv2.minAreaRect(pts.astype(np.float32))
        box = cv2.boxPoints(rect)
        cases.append(
            {
                "name": name,
                "points": [[int(p[0]), int(p[1])] for p in pts],
                "center": [float(rect[0][0]), float(rect[0][1])],
                "size": [float(rect[1][0]), float(rect[1][1])],
                "angle": float(rect[2]),
                "box": [[float(p[0]), float(p[1])] for p in box],
            }
        )

    # A rotated rectangle's lattice points.
    base = np.array([[x, y] for x in range(0, 40, 2) for y in range(0, 20, 2)], dtype=np.float64)
    ang = np.deg2rad(23.0)
    rot = np.array([[np.cos(ang), -np.sin(ang)], [np.sin(ang), np.cos(ang)]])
    rotated = (base @ rot.T + np.array([50.0, 30.0])).round().astype(np.int64)
    add_case("rotated_rect", rotated)
    add_case("noise_cloud", _RNG.randint(0, 100, (60, 2)).astype(np.int64))
    add_case("axis_rect", np.array([[x, y] for x in range(10, 31) for y in range(5, 16)]))
    _manifest("minarearect", cases)


def gen_homography() -> None:
    cases: list[dict] = []
    layout = get_layout(Region.NTSC)
    pf = layout.playfield
    dst_quad = np.array(
        [[pf.x, pf.y], [pf.x + pf.w, pf.y], [pf.x + pf.w, pf.y + pf.h], [pf.x, pf.y + pf.h]],
        dtype=np.float64,
    )

    # Exact 4-point solve.
    src_quad = np.array([[102.0, 61.5], [355.0, 74.0], [340.0, 300.0], [95.0, 288.0]])
    h_exact = cv2.getPerspectiveTransform(
        src_quad.astype(np.float32), dst_quad.astype(np.float32)
    )
    pts = np.array([[128.0, 120.0], [200.0, 90.0], [310.0, 250.0]], dtype=np.float64)
    projected = cv2.perspectiveTransform(pts.reshape(-1, 1, 2), h_exact).reshape(-1, 2)
    cases.append(
        {
            "name": "exact4",
            "kind": "getPerspectiveTransform",
            "src": src_quad.tolist(),
            "dst": dst_quad.tolist(),
            "h": np.asarray(h_exact, dtype=np.float64).ravel().tolist(),
            "transform_pts": pts.tolist(),
            "transform_out": projected.tolist(),
        }
    )

    # RANSAC sets: ground-truth H, noisy inliers, gross outliers.
    h_true = np.array(h_exact, dtype=np.float64)
    h_inv = np.linalg.inv(h_true)
    for name, n_pts, noise, n_out in [
        ("clean", 16, 0.0, 0),
        ("noisy", 24, 0.5, 0),
        ("outliers", 24, 0.5, 6),
    ]:
        dst_pts = np.column_stack(
            [_RNG.uniform(pf.x, pf.x + pf.w, n_pts), _RNG.uniform(pf.y, pf.y + pf.h, n_pts)]
        )
        src_pts = cv2.perspectiveTransform(dst_pts.reshape(-1, 1, 2), h_inv).reshape(-1, 2)
        src_pts += _RNG.normal(0, noise, src_pts.shape)
        if n_out:
            idx = _RNG.choice(n_pts, n_out, replace=False)
            src_pts[idx] = _RNG.uniform(0, 400, (n_out, 2))
        h_est, mask = cv2.findHomography(
            src_pts.astype(np.float64), dst_pts.astype(np.float64), cv2.RANSAC, 3.0
        )
        h_est = np.asarray(h_est, dtype=np.float64)
        h_norm = h_est / h_est[2, 2]
        # Mean reprojection error over the inlier set (the tolerance target).
        inl = mask.ravel().astype(bool)
        proj = cv2.perspectiveTransform(src_pts[inl].reshape(-1, 1, 2), h_est).reshape(-1, 2)
        reproj = float(np.mean(np.linalg.norm(proj - dst_pts[inl], axis=1)))
        cases.append(
            {
                "name": name,
                "kind": "findHomography_RANSAC",
                "reproj_threshold": 3.0,
                "src": src_pts.tolist(),
                "dst": dst_pts.tolist(),
                "h_true": (h_true / h_true[2, 2]).ravel().tolist(),
                "h_est": h_norm.ravel().tolist(),
                "inlier_mask": mask.ravel().astype(int).tolist(),
                "mean_inlier_reproj": reproj,
            }
        )
    _manifest("homography", cases)


def gen_warp(frames: dict[str, np.ndarray]) -> None:
    cases = []

    def add_case(name: str, image: np.ndarray) -> bool:
        result = _lock_geometry(image)
        if not result.ok or result.confidence < 0.5:
            print(f"warp: {name} did not lock (conf={getattr(result, 'confidence', 0):.2f}); skipped")
            return False
        h = np.asarray(result.homography, dtype=np.float64)
        warped_area = cv2.warpPerspective(image, h, (256, 240), flags=cv2.INTER_AREA)
        warped_linear = cv2.warpPerspective(image, h, (256, 240), flags=cv2.INTER_LINEAR)
        rectifier = Rectifier(h)
        canon = rectifier.rectify(image)
        _save(_OUT / "warp" / f"{name}_in.png", image)
        _save(_OUT / "warp" / f"{name}_warped.png", warped_area)
        _save(_OUT / "warp" / f"{name}_canon.png", canon)
        cases.append(
            {
                "name": name,
                "h": h.ravel().tolist(),
                "confidence": float(result.confidence),
                "area_equals_linear": bool(np.array_equal(warped_area, warped_linear)),
            }
        )
        return True

    add_case("tetris01", frames["tetris01"])
    for win in _WIN_CANDIDATES:
        try:
            frame = _decode_frame(win, 10.0)
        except Exception:
            continue
        if add_case("win_anamorphic", frame):
            cases[-1]["source"] = win.name
            break
    _manifest("warp", cases)
    return None


def gen_resize() -> None:
    templates = _PY_REPO / "assets" / "templates" / "common"
    lines_tpl = cv2.imread(str(templates / "labels" / "LINES.png"), cv2.IMREAD_GRAYSCALE)
    crop = _RNG.randint(0, 256, (96, 128), dtype=np.uint8)
    cases = []

    def add_case(name: str, im: np.ndarray, size: tuple[int, int]) -> None:
        out = cv2.resize(im, size, interpolation=cv2.INTER_AREA)
        _save(_OUT / "resize" / f"{name}_in.png", im)
        _save(_OUT / "resize" / f"{name}_out.png", out)
        cases.append({"name": name, "out_w": size[0], "out_h": size[1]})

    add_case("crop_half", crop, (64, 48))
    add_case("crop_frac", crop, (85, 61))
    if lines_tpl is not None:
        h, w = lines_tpl.shape
        add_case("label_down_aniso", lines_tpl, (max(1, int(w * 0.8)), max(1, int(h * 0.6))))
        # INTER_AREA upscaling silently falls back to bilinear in OpenCV.
        add_case("label_up_aniso", lines_tpl, (int(w * 1.5), int(h * 1.2)))
    _manifest("resize", cases)


def gen_undistort() -> None:
    cases = []
    for k1 in (-0.20, -0.05):
        umap = _build_map(412, 360, k1)
        ys = list(range(0, 360, 36))
        xs = list(range(0, 412, 41))
        samples = [
            {
                "x": x,
                "y": y,
                "map_x": float(umap.map1[y, x]),
                "map_y": float(umap.map2[y, x]),
            }
            for y in ys
            for x in xs
        ]
        cases.append({"name": f"k1_{k1}", "w": 412, "h": 360, "k1": k1, "samples": samples})
    _manifest("undistort", cases)


def gen_npstats() -> None:
    arr_f = _RNG.rand(137) * 100.0
    arr_u8 = _RNG.randint(0, 256, 200).astype(np.uint8)
    distinct = _RNG.permutation(50).astype(np.float64)
    cases = [
        {
            "name": "float_percentiles",
            "data": arr_f.tolist(),
            "percentiles": {str(q): float(np.percentile(arr_f, q)) for q in (10, 25, 50, 75, 90, 95)},
            "median": float(np.median(arr_f)),
            "std": float(np.std(arr_f)),
        },
        {
            "name": "u8_percentiles",
            "data": [int(v) for v in arr_u8],
            "percentiles": {str(q): float(np.percentile(arr_u8, q)) for q in (50, 90)},
            "median": float(np.median(arr_u8)),
            "std": float(np.std(arr_u8)),
        },
        {
            "name": "argsort_distinct",
            "data": distinct.tolist(),
            "argsort": [int(i) for i in np.argsort(distinct)],
        },
    ]
    _manifest("npstats", cases)


def main() -> None:
    frames = _reference_frames()
    # A rectified canonical frame for recognition-adjacent goldens.
    canon = None
    result = _lock_geometry(frames["tetris01"])
    if result.ok:
        canon = Rectifier(np.asarray(result.homography)).rectify(frames["tetris01"])
        _save(_OUT / "canon_tetris01.png", canon)
    gen_color(frames)
    gen_otsu(frames, canon)
    gen_morph()
    gen_ncc(canon)
    gen_components()
    gen_min_area_rect()
    gen_homography()
    gen_warp(frames)
    gen_resize()
    gen_undistort()
    gen_npstats()
    print(f"goldens written to {_OUT}")


if __name__ == "__main__":
    main()
