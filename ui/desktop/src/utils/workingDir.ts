export const getInitialWorkingDir = (): string => {
  // Fall back to initial config from app startup
  return (window.appConfig?.get('LUMINA_WORKING_DIR') as string) ?? '';
};
