"""Small text screen for the cursor/erase sequences emitted by our PTY tests.

Assertions use rendered cells rather than assuming differential terminal writes
contain a whole word in one byte sequence. This is not a terminal emulator used
by the product and intentionally ignores colour, modes and other non-text state.
"""
import codecs
import re
import unicodedata


class TextScreen:
    def __init__(self, columns, rows):
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.pending = ''
        self.resize(columns, rows)

    def resize(self, columns, rows):
        self.columns, self.rows = columns, rows
        self.cells = [[' '] * columns for _ in range(rows)]
        self.x = self.y = 0

    def text(self):
        return '\n'.join(''.join(row) for row in self.cells)

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            if self.pending.startswith('\x1b['):
                match = re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])', self.pending)
                if match is None:
                    return
                self.pending = self.pending[match.end():]
                raw, _, command = match.groups()
                values = [int(v or 0) for v in raw.lstrip('?').split(';')]
                n = values[0] or 1
                if command in ('H', 'f'):
                    self.y = min(self.rows - 1, n - 1)
                    self.x = min(self.columns - 1, (values[1] or 1) - 1 if len(values) > 1 else 0)
                elif command == 'G':
                    self.x = min(self.columns - 1, n - 1)
                elif command == 'A':
                    self.y = max(0, self.y - n)
                elif command == 'B':
                    self.y = min(self.rows - 1, self.y + n)
                elif command == 'C':
                    self.x = min(self.columns - 1, self.x + n)
                elif command == 'D':
                    self.x = max(0, self.x - n)
                elif command == 'J' and values[0] in (2, 3):
                    self.cells = [[' '] * self.columns for _ in range(self.rows)]
                elif command == 'K':
                    start, end = (0, self.columns) if values[0] == 2 else ((0, self.x + 1) if values[0] == 1 else (self.x, self.columns))
                    self.cells[self.y][start:end] = [' '] * (end - start)
                continue
            if self.pending == '\x1b':
                return
            char, self.pending = self.pending[0], self.pending[1:]
            if char == '\r':
                self.x = 0
            elif char == '\n':
                self.y = min(self.rows - 1, self.y + 1)
            elif char == '\b':
                self.x = max(0, self.x - 1)
            elif not char.isprintable() or unicodedata.combining(char):
                continue
            else:
                if self.x >= self.columns:
                    self.x, self.y = 0, min(self.rows - 1, self.y + 1)
                width = 2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1
                self.cells[self.y][self.x] = char
                if width == 2 and self.x + 1 < self.columns:
                    self.cells[self.y][self.x + 1] = ''
                self.x += width
