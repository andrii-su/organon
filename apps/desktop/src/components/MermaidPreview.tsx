import { useEffect, useId, useState } from "react";

interface MermaidPreviewProps {
  chart: string;
}

export function MermaidPreview({ chart }: MermaidPreviewProps) {
  const id = useId().replace(/:/g, "-");
  const [svg, setSvg] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    import("mermaid")
      .then(async ({ default: mermaid }) => {
        mermaid.initialize({
          startOnLoad: false,
          theme: "base",
          themeVariables: {
            background: "#f4efe6",
            primaryColor: "#d8e1d0",
            primaryTextColor: "#18222b",
            primaryBorderColor: "#7b8f79",
            lineColor: "#3d5366",
            secondaryColor: "#eadfcb",
            tertiaryColor: "#f9f5ee",
          },
        });

        return mermaid.render(`organon-mermaid-${id}`, chart);
      })
      .then((result) => {
        if (!cancelled) {
          setSvg(result.svg);
          setError(null);
        }
      })
      .catch((renderError: unknown) => {
        if (!cancelled) {
          setSvg("");
          setError(
            renderError instanceof Error
              ? renderError.message
              : "Mermaid render failed.",
          );
        }
      });

    return () => {
      cancelled = true;
    };
  }, [chart, id]);

  if (error) {
    return (
      <div className="state-card error">
        <strong>Mermaid preview failed.</strong>
        <span>{error}</span>
      </div>
    );
  }

  if (!svg) {
    return <div className="state-card">Rendering graph…</div>;
  }

  return (
    <div
      className="mermaid-preview"
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
}
