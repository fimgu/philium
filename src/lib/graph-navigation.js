// Explicit wheel semantics shared by mouse wheels, trackpads and pinch.
export function wheelZoomFactor(event, height = 800) {
  const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? height : 1;
  const delta = Number(event.deltaY) * unit;
  if (!Number.isFinite(delta)) return 1;
  return Math.exp(Math.max(-1, Math.min(1, -delta * (event.ctrlKey ? 0.012 : 0.003))));
}

export function zoomCamera(camera, factor, x, y) {
  const k = Math.max(0.04, Math.min(8, camera.k * factor));
  const ratio = k / camera.k;
  return { k, x: x - (x - camera.x) * ratio, y: y - (y - camera.y) * ratio };
}
