export type ReviewerTheme = "light" | "dark";

export const REVIEWER_THEME_STORAGE_KEY = "cvc-reviewer-theme";

export function isReviewerTheme(value: string | null | undefined): value is ReviewerTheme {
  return value === "light" || value === "dark";
}

export function getReviewerTheme(): ReviewerTheme {
  if (typeof document === "undefined") {
    return "light";
  }

  return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
}

export function applyReviewerTheme(theme: ReviewerTheme) {
  if (typeof document === "undefined") {
    return theme;
  }

  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;

  try {
    window.localStorage.setItem(REVIEWER_THEME_STORAGE_KEY, theme);
  } catch {
    // Ignore storage failures and still apply the theme.
  }

  return theme;
}

export function initializeReviewerTheme() {
  try {
    const storedTheme = window.localStorage.getItem(REVIEWER_THEME_STORAGE_KEY);
    const theme = isReviewerTheme(storedTheme)
      ? storedTheme
      : (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");

    return applyReviewerTheme(theme);
  } catch {
    return applyReviewerTheme("light");
  }
}

export function toggleReviewerTheme() {
  const nextTheme: ReviewerTheme = getReviewerTheme() === "dark" ? "light" : "dark";
  return applyReviewerTheme(nextTheme);
}
