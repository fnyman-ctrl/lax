import { useCallback, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * A newest-first list read a page at a time from a command that takes
 * `limit` and, to continue, `beforeAt` and `beforeId` (the captures and the
 * journal). `at` is the time an item is ordered by.
 *
 * `refresh` fetches the newest page and replaces what it covers, so new items
 * appear and deleted ones disappear, and keeps older pages already loaded.
 * `loadOlder` adds the next page. Answers that arrive after a newer request
 * are dropped, so a slow request can't bring back what a later one removed.
 */
export function usePaged<T extends { id: number }>(
  command: string,
  at: (item: T) => number,
  page: number,
) {
  const [items, setItems] = useState<T[] | null>(null);
  // The newest page couldn't be read (on the last refresh).
  const [failed, setFailed] = useState(false);
  const [olderFailed, setOlderFailed] = useState(false);
  const [more, setMore] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const itemsRef = useRef<T[]>([]);
  const latestRefresh = useRef(0);

  const show = (next: T[]) => {
    itemsRef.current = next;
    setItems(next);
  };

  const refresh = useCallback(() => {
    const request = ++latestRefresh.current;
    invoke<T[]>(command, { limit: page }).then(
      (newest) => {
        if (request !== latestRefresh.current) {
          return;
        }
        setFailed(false);
        if (newest.length < page) {
          // That is everything there is.
          setMore(false);
          show(newest);
          return;
        }
        const last = newest[newest.length - 1];
        const older = itemsRef.current.filter(
          (item) => at(item) < at(last) || (at(item) === at(last) && item.id < last.id),
        );
        if (older.length === 0) {
          setMore(true);
        }
        show([...newest, ...older]);
      },
      () => {
        if (request === latestRefresh.current) {
          setFailed(true);
        }
      },
    );
  }, [command, at, page]);

  const loadOlder = () => {
    const last = itemsRef.current[itemsRef.current.length - 1];
    if (!last) {
      return;
    }
    setLoadingMore(true);
    setOlderFailed(false);
    invoke<T[]>(command, { beforeAt: at(last), beforeId: last.id, limit: page })
      .then(
        (older) => {
          // A refresh replaced the end of the list meanwhile (new items
          // pushed it down, or items were deleted): this page may not
          // follow on any more, so leave it to the next click.
          if (itemsRef.current[itemsRef.current.length - 1] !== last) {
            return;
          }
          setMore(older.length === page);
          show([...itemsRef.current, ...older]);
        },
        () => setOlderFailed(true),
      )
      .finally(() => setLoadingMore(false));
  };

  return { items, failed, olderFailed, more, loadingMore, refresh, loadOlder };
}
