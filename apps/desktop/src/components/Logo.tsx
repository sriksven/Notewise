interface Props {
  size?: number;
  className?: string;
}

/**
 * The mark.
 *
 * A pen nib over a waveform, drawn rather than imported: a mark that has to be legible at 18px
 * in a 52px rail should be a handful of vectors, not a raster asset with a light and a dark
 * variant to keep in sync. It inherits `currentColor`, so it follows the theme for free.
 */
export function Logo({ size = 20, className }: Props) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      className={className}
      aria-hidden
    >
      {/* The waveform: what was heard. */}
      <path
        d="M3 12v0M6.5 8.5v7M10 6v12M13.5 9.5v5"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        opacity="0.45"
      />
      {/* The nib: what it became. */}
      <path
        d="M20.5 4.2 16 8.7l-1 3.6 3.6-1 4.5-4.5a1.6 1.6 0 0 0 0-2.3l-.3-.3a1.6 1.6 0 0 0-2.3 0Z"
        fill="currentColor"
      />
    </svg>
  );
}
