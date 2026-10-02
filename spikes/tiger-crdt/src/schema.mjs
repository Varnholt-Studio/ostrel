// The benchmark issue model of MEASUREMENT 2.3, reduced to the fields the probes touch.
export const STATUSES = ['backlog', 'todo', 'doing', 'review', 'done'];
export const PRIORITIES = ['none', 'low', 'medium', 'high', 'urgent'];

// kind: lww value type, `set` for Set[Text], `seq` for Text merge text.
export const FIELDS = {
  title: { kind: 'text' },
  status: { kind: 'enum', values: STATUSES },
  priority: { kind: 'enum', values: PRIORITIES },
  assignee: { kind: 'text' },
  rank: { kind: 'rank' },
  labels: { kind: 'set' },
  desc: { kind: 'seq' },
};

export const MAX_TEXT_BYTES = 256 * 1024;
export const MAX_OPS_PER_BATCH = 500;
export const MAX_REMOVE_TAGS = 64;
