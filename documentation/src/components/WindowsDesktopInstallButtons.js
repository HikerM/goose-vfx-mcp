import Link from "@docusaurus/Link";
import { IconDownload } from "@site/src/components/icons/download";

const WindowsDesktopInstallButtons = () => {
  return (
    <div>
      <p>Click one of the buttons below to download lumina Desktop for Windows:</p>
      <div className="pill-button" style={{ display: "flex", gap: "0.75rem", flexWrap: "wrap" }}>
        <Link
          className="button button--primary button--lg"
          to="https://github.com/HikerM/lumina/releases/download/stable/Lumina-win32-x64.zip"
        >
          <IconDownload /> Windows
        </Link>
      </div>
    </div>
  );
};

export default WindowsDesktopInstallButtons;
