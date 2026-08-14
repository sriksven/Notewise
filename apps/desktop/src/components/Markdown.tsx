import { useMemo } from "react";

import { parseMarkdown, type Span } from "../lib/markdown";

interface Props {
  source: string;
}

function spans(parts: Span[]) {
  return parts.map((part, index) =>
    part.bold ? (
      <strong key={index} className="font-semibold text-neutral-900">
        {part.text}
      </strong>
    ) : (
      <span key={index}>{part.text}</span>
    ),
  );
}

/**
 * A summary, rendered as prose.
 *
 * Text only — the parser produces spans with a bold flag and nothing else, so there is no path
 * from a model's output to markup. That matters more here than anywhere else in the app: the
 * content is generated from whatever was said in a meeting.
 */
export function Markdown({ source }: Props) {
  const blocks = useMemo(() => parseMarkdown(source), [source]);

  return (
    <div className="space-y-3">
      {blocks.map((block, index) => {
        if (block.kind === "heading") {
          return (
            <h3
              key={index}
              className="pt-1 text-[13px] font-semibold tracking-tight text-neutral-900"
            >
              {spans(block.spans)}
            </h3>
          );
        }

        if (block.kind === "list") {
          return (
            <ul key={index} className="space-y-1.5">
              {block.items.map((item, itemIndex) => (
                <li key={itemIndex} className="flex items-start gap-2">
                  <span className="mt-[7px] h-1 w-1 shrink-0 rounded-full bg-neutral-300" />
                  <span className="text-[14px] leading-relaxed text-neutral-700">
                    {spans(item)}
                  </span>
                </li>
              ))}
            </ul>
          );
        }

        return (
          <p key={index} className="text-[14px] leading-relaxed text-neutral-700">
            {spans(block.spans)}
          </p>
        );
      })}
    </div>
  );
}
