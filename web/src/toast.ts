// Small auto-expiring toast notifications (top-right stack).

export type ToastKind = "info" | "success" | "error";

export function showToast(kind: ToastKind, text: string): void {
  const container = document.getElementById("toasts")!;
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = text;
  container.appendChild(el);
  while (container.children.length > 6) {
    container.firstElementChild?.remove();
  }
  setTimeout(() => el.remove(), kind === "error" ? 8000 : 4000);
}
