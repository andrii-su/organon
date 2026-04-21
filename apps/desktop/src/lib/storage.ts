import { useEffect, useState } from "react";

export function useStoredState<T>(key: string, initialValue: T) {
  const [value, setValue] = useState<T>(() => {
    const raw = window.localStorage.getItem(key);
    if (!raw) {
      return initialValue;
    }

    try {
      return JSON.parse(raw) as T;
    } catch {
      return initialValue;
    }
  });

  useEffect(() => {
    window.localStorage.setItem(key, JSON.stringify(value));
  }, [key, value]);

  return [value, setValue] as const;
}
