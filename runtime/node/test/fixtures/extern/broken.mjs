// Extern module that fails while loading, so the host must exit with code 2 before `ready`.
throw new Error('broken on load');
