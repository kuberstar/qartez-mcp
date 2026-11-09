import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import { createTV } from "tailwind-variants/lite";

export function cn(...inputs: ClassValue[]) {
	return twMerge(clsx(inputs));
}

// tailwind-variants 4 bundles its own merge engine in the default entry;
// the lite entry reuses the tailwind-merge that `cn` already ships.
export const tv = createTV({ twMerge });

export type WithElementRef<T, U extends HTMLElement = HTMLElement> = T & { ref?: U | null };
