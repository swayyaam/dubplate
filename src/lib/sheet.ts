import { useEffect } from "react";

/**
 * Escape closes a sheet.
 *
 * Registered on the document rather than the panel because a dialog that can
 * only be dismissed by finding the Cancel button is a dialog people feel
 * trapped in, and focus may legitimately be anywhere inside it.
 */
export function useEscape(onClose: () => void) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [onClose]);
}
