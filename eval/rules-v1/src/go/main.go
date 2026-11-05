package ledger

import "fmt"

func Run() {
	l := &Ledger{entries: map[string]int{}}
	l.Add("a", 1)
	v, _ := l.Get("a")
	fmt.Println(v)
	// ast-grep-ignore: go-println
	fmt.Println("done")
	go func() {
		fmt.Printf("%d\n", v)
	}()
}
