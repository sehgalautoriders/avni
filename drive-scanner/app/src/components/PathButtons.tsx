import { useMutation } from '@tanstack/react-query';
import { api } from '../api';
import { Button, ErrorText } from './ui';

/** "Open" and "Show in folder" for one path, each with its own busy state and inline error. */
export function PathButtons({
  path,
  openLabel = 'Open',
  showFolder = true,
}: {
  path: string;
  openLabel?: string;
  showFolder?: boolean;
}) {
  const open = useMutation({ mutationFn: () => api.openPath(path) });
  const reveal = useMutation({ mutationFn: () => api.revealPath(path) });
  return (
    <div className="flex flex-col gap-1">
      <div className="flex flex-wrap gap-2">
        <Button small busy={open.isPending} onClick={() => open.mutate()}>
          {openLabel}
        </Button>
        {showFolder && (
          <Button small busy={reveal.isPending} onClick={() => reveal.mutate()}>
            Show in folder
          </Button>
        )}
      </div>
      <ErrorText error={open.error ?? reveal.error} />
    </div>
  );
}
