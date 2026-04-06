import { useEffect } from "react";

function isEditableElement(target: EventTarget | null): boolean {
  if (!(target instanceof Node)) {
    return false;
  }

  let current: Node | null = target;

  while (current) {
    if (current instanceof HTMLInputElement || current instanceof HTMLTextAreaElement) {
      return true;
    }

    if (current instanceof HTMLElement && current.isContentEditable) {
      return true;
    }

    if (current instanceof ShadowRoot) {
      current = current.host;
      continue;
    }

    current = current.parentNode;
  }

  return false;
}

export function useDisableDefaultContextMenu() {
  useEffect(() => {
    const handleContextMenu = (event: MouseEvent) => {
      if (isEditableElement(event.target)) {
        return;
      }

      event.preventDefault();
    };

    document.addEventListener("contextmenu", handleContextMenu);

    return () => {
      document.removeEventListener("contextmenu", handleContextMenu);
    };
  }, []);
}
