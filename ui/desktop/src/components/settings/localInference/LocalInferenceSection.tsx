import { LocalInferenceSettings } from './LocalInferenceSettings';
import { InstalledTaskParameters } from './InstalledTaskParameters';

export default function LocalInferenceSection({ sessionIds }: { sessionIds: string[] }) {
  return (
    <section id="local-inference" className="space-y-4 pr-4 pb-8">
      <LocalInferenceSettings />
      <InstalledTaskParameters sessionIds={sessionIds} />
    </section>
  );
}
