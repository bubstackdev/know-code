package ledger

import (
	"errors"
	"fmt"
	"sync"
)

type Ledger struct {
	mu      sync.Mutex
	entries map[string]int
}

func (l *Ledger) Add(key string, n int) {
	l.mu.Lock()
	l.entries[key] += n
	l.mu.Unlock()
	fmt.Println("added", key)
}

func (l *Ledger) Get(key string) (int, error) {
	v, ok := l.entries[key]
	if !ok {
		return 0, errors.New("missing " + key)
	}
	return v, nil
}

func Must(v int, err error) int {
	if err != nil {
		panic(err)
	}
	return v
}
