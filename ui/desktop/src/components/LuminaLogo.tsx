import luminaLogo from '../images/lumina-logo.png';
import { cn } from '../utils';

interface LuminaLogoProps {
  className?: string;
  size?: 'default' | 'small' | 'tiny';
  hover?: boolean;
}

export default function LuminaLogo({
  className = '',
  size = 'default',
  hover = true,
}: LuminaLogoProps) {
  const sizes = {
    default: 'size-16',
    small: 'size-8',
    tiny: 'size-5',
  } as const;

  return (
    <img
      src={luminaLogo}
      alt="Lumina"
      draggable={false}
      className={cn(
        className,
        sizes[size],
        'object-contain select-none',
        hover && 'transition-transform duration-300 hover:scale-105'
      )}
    />
  );
}
