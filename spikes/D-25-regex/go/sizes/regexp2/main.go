package main

import (
	"fmt"

	"github.com/dlclark/regexp2/v2"
)

func main() {
	re := regexp2.MustCompile(`(?<=a)b`, regexp2.ECMAScript)
	ok, _ := re.MatchString("ab")
	fmt.Println(ok)
}
