import { useState, useEffect } from 'react';
import { Button } from '../../ui/button';
import { Check } from '../../icons';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../../ui/dialog';
import { errorMessage } from '../../../utils/conversionUtils';
import { defineMessages, useIntl } from '../../../i18n';
import { PRIMARY_DOCS_URL } from '../../../distribution-config';

const i18n = defineMessages({
  dialogTitle: {
    id: 'luminahintsModal.dialogTitle',
    defaultMessage: 'Configure Project Hints (.luminahints)',
  },
  dialogDescription: {
    id: 'luminahintsModal.dialogDescription',
    defaultMessage:
      'Provide additional context about your project to improve communication with Lumina',
  },
  helpText1: {
    id: 'luminahintsModal.helpText1',
    defaultMessage:
      '.luminahints is a text file used to provide additional context about your project and improve the communication with Lumina.',
  },
  helpText2: {
    id: 'luminahintsModal.helpText2',
    defaultMessage:
      "Please make sure {bold} extension is enabled in the extensions page. This extension is required to use .luminahints. You'll need to restart your session for .luminahints updates to take effect.",
  },
  helpText3: {
    id: 'luminahintsModal.helpText3',
    defaultMessage: 'See {link} for more information.',
  },
  helpTextLink: {
    id: 'luminahintsModal.helpTextLink',
    defaultMessage: 'using .luminahints',
  },
  errorReading: {
    id: 'luminahintsModal.errorReading',
    defaultMessage: 'Error reading .luminahints file: {error}',
  },
  fileFound: {
    id: 'luminahintsModal.fileFound',
    defaultMessage: '.luminahints file found at: {filePath}',
  },
  fileCreating: {
    id: 'luminahintsModal.fileCreating',
    defaultMessage: 'Creating new .luminahints file at: {filePath}',
  },
  placeholder: {
    id: 'luminahintsModal.placeholder',
    defaultMessage: 'Enter project hints here...',
  },
  savedSuccessfully: {
    id: 'luminahintsModal.savedSuccessfully',
    defaultMessage: 'Saved successfully',
  },
  close: {
    id: 'luminahintsModal.close',
    defaultMessage: 'Close',
  },
  saving: {
    id: 'luminahintsModal.saving',
    defaultMessage: 'Saving...',
  },
  save: {
    id: 'luminahintsModal.save',
    defaultMessage: 'Save',
  },
  failedToAccess: {
    id: 'luminahintsModal.failedToAccess',
    defaultMessage: 'Failed to access .luminahints file',
  },
  failedToSave: {
    id: 'luminahintsModal.failedToSave',
    defaultMessage: 'Failed to save .luminahints file',
  },
  developer: {
    id: 'luminahintsModal.developer',
    defaultMessage: 'Developer',
  },
});

const HelpText = () => {
  const intl = useIntl();

  return (
    <div className="text-sm flex-col space-y-4 text-text-secondary">
      <p>{intl.formatMessage(i18n.helpText1)}</p>
      <p>
        {intl.formatMessage(i18n.helpText2, {
          bold: <span className="font-bold">{intl.formatMessage(i18n.developer)}</span>,
        })}
      </p>
      <p>
        {intl.formatMessage(i18n.helpText3, {
          link: PRIMARY_DOCS_URL ? (
            <Button
              variant="link"
              className="text-blue-500 hover:text-blue-600 p-0 h-auto"
              onClick={() => window.open(`${PRIMARY_DOCS_URL}/guides/using-luminahints/`, '_blank')}
            >
              {intl.formatMessage(i18n.helpTextLink)}
            </Button>
          ) : (
            <span>{intl.formatMessage(i18n.helpTextLink)}</span>
          ),
        })}
      </p>
    </div>
  );
};

const ErrorDisplay = ({ error }: { error: Error }) => {
  const intl = useIntl();

  return (
    <div className="text-sm text-text-secondary">
      <div className="text-red-600">
        {intl.formatMessage(i18n.errorReading, { error: errorMessage(error) })}
      </div>
    </div>
  );
};

const FileInfo = ({ filePath, found }: { filePath: string; found: boolean }) => {
  const intl = useIntl();

  return (
    <div className="text-sm font-medium mb-2">
      {found ? (
        <div className="text-green-600">
          <Check className="w-4 h-4 inline-block" />{' '}
          {intl.formatMessage(i18n.fileFound, { filePath })}
        </div>
      ) : (
        <div>{intl.formatMessage(i18n.fileCreating, { filePath })}</div>
      )}
    </div>
  );
};

const getLuminahintsFile = async (directory: string) => {
  const access = await window.electron.getProjectDirectoryAccess();
  const sameDirectory =
    access.directory?.replace(/\\/g, '/').replace(/\/$/, '').toLowerCase() ===
    directory.replace(/\\/g, '/').replace(/\/$/, '').toLowerCase();
  if (
    (!access.authorized || !sameDirectory) &&
    !(await window.electron.requestProjectDirectoryAccess(directory))
  ) {
    return { file: '', error: 'Project directory access is not authorized', found: false };
  }
  const currentAccess = await window.electron.getProjectDirectoryAccess();
  return currentAccess.token
    ? window.electron.readProjectLuminahints(currentAccess.token, directory)
    : { file: '', error: 'Project directory access is not authorized', found: false };
};

interface LuminahintsModalProps {
  directory: string;
  setIsLuminahintsModalOpen: (isOpen: boolean) => void;
}

export const LuminahintsModal = ({
  directory,
  setIsLuminahintsModalOpen,
}: LuminahintsModalProps) => {
  const intl = useIntl();
  const luminahintsFilePath = `${directory}/.luminahints`;
  const [luminahintsFile, setLuminahintsFile] = useState<string>('');
  const [luminahintsFileFound, setLuminahintsFileFound] = useState<boolean>(false);
  const [luminahintsFileReadError, setLuminahintsFileReadError] = useState<string>('');
  const [isSaving, setIsSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);

  useEffect(() => {
    const fetchLuminahintsFile = async () => {
      try {
        const { file, error, found } = await getLuminahintsFile(directory);
        setLuminahintsFile(file);
        setLuminahintsFileFound(found);
        setLuminahintsFileReadError(found && error ? error : '');
      } catch (error) {
        console.error('Error fetching .luminahints file:', error);
        setLuminahintsFileReadError(intl.formatMessage(i18n.failedToAccess));
      }
    };
    if (directory) fetchLuminahintsFile();
  }, [directory, luminahintsFilePath, intl]);

  const writeFile = async () => {
    setIsSaving(true);
    setSaveSuccess(false);
    try {
      const access = await window.electron.getProjectDirectoryAccess();
      const sameDirectory =
        access.directory?.replace(/\\/g, '/').replace(/\/$/, '').toLowerCase() ===
        directory.replace(/\\/g, '/').replace(/\/$/, '').toLowerCase();
      if (
        (!access.authorized || !sameDirectory) &&
        !(await window.electron.requestProjectDirectoryAccess(directory))
      ) {
        throw new Error('Project directory access is not authorized');
      }
      const currentAccess = await window.electron.getProjectDirectoryAccess();
      if (
        !currentAccess.token ||
        !(await window.electron.writeProjectLuminahints(
          luminahintsFile,
          currentAccess.token,
          directory
        ))
      ) {
        throw new Error('Unable to write project hints');
      }
      setSaveSuccess(true);
      setLuminahintsFileFound(true);
      setTimeout(() => setSaveSuccess(false), 3000);
    } catch (error) {
      console.error('Error writing .luminahints file:', error);
      setLuminahintsFileReadError(intl.formatMessage(i18n.failedToSave));
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <Dialog open={true} onOpenChange={(open) => setIsLuminahintsModalOpen(open)}>
      <DialogContent className="w-[80vw] max-w-[80vw] sm:max-w-[80vw] max-h-[90vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage(i18n.dialogTitle)}</DialogTitle>
          <DialogDescription>{intl.formatMessage(i18n.dialogDescription)}</DialogDescription>
        </DialogHeader>

        <div className="flex-1 overflow-y-auto space-y-4 pt-2 pb-4">
          <HelpText />

          <div>
            {luminahintsFileReadError ? (
              <ErrorDisplay error={new Error(luminahintsFileReadError)} />
            ) : (
              <div className="space-y-2">
                <FileInfo filePath={luminahintsFilePath} found={luminahintsFileFound} />
                <textarea
                  value={luminahintsFile}
                  className="w-full h-80 border rounded-md p-2 text-sm resize-none bg-background-primary text-text-primary border-border-primary focus:outline-none focus:ring-2 focus:ring-blue-500"
                  onChange={(event) => setLuminahintsFile(event.target.value)}
                  placeholder={intl.formatMessage(i18n.placeholder)}
                />
              </div>
            )}
          </div>
        </div>

        <DialogFooter>
          {saveSuccess && (
            <span className="text-green-600 text-sm flex items-center gap-1 mr-auto">
              <Check className="w-4 h-4" />
              {intl.formatMessage(i18n.savedSuccessfully)}
            </span>
          )}
          <Button variant="outline" onClick={() => setIsLuminahintsModalOpen(false)}>
            {intl.formatMessage(i18n.close)}
          </Button>
          <Button onClick={writeFile} disabled={isSaving}>
            {isSaving ? intl.formatMessage(i18n.saving) : intl.formatMessage(i18n.save)}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};
