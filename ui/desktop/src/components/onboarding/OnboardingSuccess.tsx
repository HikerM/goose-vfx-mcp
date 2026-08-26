import { Button } from '../ui/button';
import { defineMessages, useIntl } from '../../i18n';

const LOCAL_PROVIDER = 'local';

const i18n = defineMessages({
  localModelReady: {
    id: 'onboardingSuccess.localModelReady',
    defaultMessage: 'Local model ready',
  },
  connectedTo: {
    id: 'onboardingSuccess.connectedTo',
    defaultMessage: 'Connected to {providerName}',
  },
  allSet: {
    id: 'onboardingSuccess.allSet',
    defaultMessage: "You're all set to start using Lumina.",
  },
  privacyTitle: {
    id: 'onboardingSuccess.privacyTitle',
    defaultMessage: 'Privacy',
  },
  privacyDescription: {
    id: 'onboardingSuccess.noTelemetryDescription',
    defaultMessage:
      'Lumina does not send telemetry. Prompts are only sent to the model provider you choose, or remain local when you use a local model.',
  },
  getStarted: {
    id: 'onboardingSuccess.getStarted',
    defaultMessage: 'Get Started',
  },
});

interface OnboardingSuccessProps {
  providerName: string;
  onFinish: () => void;
}

export default function OnboardingSuccess({ providerName, onFinish }: OnboardingSuccessProps) {
  const intl = useIntl();

  return (
    <div className="h-screen w-full bg-background-default overflow-hidden">
      <div className="h-full overflow-y-auto">
        <div className="flex flex-col items-center justify-center h-full p-4">
          <div className="max-w-md w-full mx-auto text-center">
            <div className="mb-6">
              <div className="inline-flex items-center justify-center w-12 h-12 rounded-full bg-green-500/10 mb-4">
                <svg
                  className="w-6 h-6 text-green-500"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M5 13l4 4L19 7"
                  />
                </svg>
              </div>
              <h2 className="text-xl font-light text-text-default mb-1">
                {providerName === LOCAL_PROVIDER
                  ? intl.formatMessage(i18n.localModelReady)
                  : intl.formatMessage(i18n.connectedTo, { providerName })}
              </h2>
              <p className="text-text-muted text-sm">{intl.formatMessage(i18n.allSet)}</p>
            </div>

            <div className="w-full p-4 bg-transparent border rounded-xl text-left mb-6">
              <h3 className="font-medium text-text-default text-sm mb-1">
                {intl.formatMessage(i18n.privacyTitle)}
              </h3>
              <p className="text-text-muted text-sm">
                {intl.formatMessage(i18n.privacyDescription)}
              </p>
            </div>

            <Button onClick={onFinish} className="w-full">
              {intl.formatMessage(i18n.getStarted)}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
