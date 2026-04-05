import { version } from '../../package.json';

export type InstallMode = 'git' | 'npm';

export const useVersionCheck = (_owner: string, _repo: string) => {
  return {
    updateAvailable: false,
    latestVersion: null as string | null,
    currentVersion: version,
    releaseInfo: null,
    installMode: 'git' as InstallMode,
  };
};
