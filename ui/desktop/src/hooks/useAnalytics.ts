/** Local-only UI event hooks. The compatibility sink never stores or transmits data. */

import { useEffect, useRef } from 'react';
import { useLocation } from 'react-router-dom';
import { trackPageView } from '../utils/analytics';

export function usePageViewTracking(): void {
  const location = useLocation();
  const previousPath = useRef<string | null>(null);

  useEffect(() => {
    const currentPath = location.pathname;
    if (currentPath !== previousPath.current) {
      trackPageView(currentPath, previousPath.current || undefined);
      previousPath.current = currentPath;
    }
  }, [location.pathname]);
}
