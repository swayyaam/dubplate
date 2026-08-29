import { memo, useState } from "react";
import { artUrl, type ArtSize } from "../lib/art";

interface Props {
  hash: string | null;
  size: ArtSize;
  alt: string;
  className?: string;
}

/**
 * An album cover, or a placeholder that keeps the same square so a grid never
 * reflows while art loads.
 */
export const Cover = memo(function Cover({ hash, size, alt, className }: Props) {
  const [failed, setFailed] = useState(false);
  const url = failed ? null : artUrl(hash, size);

  return (
    <div className={`cover${className ? ` ${className}` : ""}`}>
      {url ? (
        <img
          className="cover__image"
          src={url}
          alt={alt}
          loading="lazy"
          decoding="async"
          draggable={false}
          onError={() => setFailed(true)}
        />
      ) : (
        <div className="cover__blank" aria-label={alt} />
      )}
    </div>
  );
});
