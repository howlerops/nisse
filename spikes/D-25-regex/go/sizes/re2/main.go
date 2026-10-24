package main

import (
	"fmt"
	"regexp"
)

func main() { fmt.Println(regexp.MustCompile(`a+b`).MatchString("aab")) }
