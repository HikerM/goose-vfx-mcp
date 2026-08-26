import { useColorMode } from '@docusaurus/theme-common';

export const LuminaLogo = (props: { className?: string }) => {
  const { colorMode } = useColorMode();

  const logoSrc = colorMode === 'dark'
    ? 'img/lumina-logo-white.png'
    : 'img/lumina-logo-black.png';

  const logoAlt = 'Lumina logo';

  return (
    <img
      src={logoSrc}
      alt={logoAlt}
      className={props.className}
      style={{ height: 'auto', maxWidth: '100%' }}
    />
  );
};
