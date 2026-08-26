import LuminaLogo from './LuminaLogo';

interface LuminaActivityProps {
  className?: string;
}

export default function LuminaActivity({ className = '' }: LuminaActivityProps) {
  return (
    <span
      className={`relative inline-flex size-5 shrink-0 items-center justify-center ${className}`}
      aria-hidden="true"
    >
      <span className="absolute inset-0 animate-ping rounded-full bg-orange-400/25" />
      <LuminaLogo className="relative animate-pulse" size="tiny" hover={false} />
    </span>
  );
}
