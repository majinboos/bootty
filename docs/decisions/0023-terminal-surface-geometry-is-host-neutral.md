# Terminal surface geometry is host-neutral

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-surface` owns numeric terminal geometry. Host adapters convert their UI
coordinates at the boundary.

## Authority and invariants

- `SurfaceRect` owns one logical rectangle.
- `SurfacePoint` owns one logical position.
- `TerminalSurface` stores a `SurfaceRect`.
- Logical-size constructors accept scalar width and height values.
- `ViewTransform` accepts `SurfacePoint` positions and scalar pan deltas.
- `bootty-app` and `bootty-winit` convert egui values before they call surface
  geometry.
- Grid dimensions keep floor rounding.
- Cell dimensions keep ceiling rounding.
- Padding keeps nearest-integer rounding.
- Pointer mapping keeps rendered floating metrics and rounded protocol metrics.
- Rectangle containment keeps the current minimum-inclusive and
  maximum-inclusive behavior.

## Simplification

`bootty-surface` does not depend on `eframe` or expose egui convenience
constructors. The existing `SurfaceRect` and `SurfacePoint` values replace
`egui::Rect` and `egui::Pos2`. Scalar dimensions replace `egui::Vec2`, so no
new size or delta DTO is needed.

## Dependency direction

The terminal, runtime, renderer, site, tests, and benchmarks consume numeric
surface geometry. The app and winit adapters retain their host-specific egui
input types and perform direct field conversion.

## Rejected alternatives

- Keeping `eframe` optional still lets a low-level geometry value expose host
  UI types.
- A shared UI geometry trait would have one real host implementation.
- A new `SurfaceSize` or `SurfaceDelta` would wrap two scalar values without
  hiding policy.
- Removing egui from app or winit input is outside this boundary.
