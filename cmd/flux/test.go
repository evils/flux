package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	arrowmem "github.com/apache/arrow/go/v7/arrow/memory"
	"github.com/influxdata/flux"
	"github.com/influxdata/flux/ast"
	"github.com/influxdata/flux/cmd/flux/cmd"
	"github.com/influxdata/flux/codes"
	"github.com/influxdata/flux/dependencies/testing"
	"github.com/influxdata/flux/dependency"
	"github.com/influxdata/flux/execute/executetest"
	"github.com/influxdata/flux/execute/table"
	"github.com/influxdata/flux/internal/errors"
	"github.com/influxdata/flux/lang"
	"github.com/influxdata/flux/memory"
	"github.com/influxdata/flux/runtime"
)

func NewTestExecutor(ctx context.Context) (cmd.TestExecutor, error) {
	return testExecutor{}, nil
}

type consoleLogger struct {
	errs int
}

func (c *consoleLogger) Errorf(format string, args ...interface{}) {
	_, _ = fmt.Fprintf(os.Stderr, format, args...)
	c.errs++
}

func (c *consoleLogger) Helper() {}

type testExecutor struct{}

func (testExecutor) Run(pkg *ast.Package) error {
	jsonAST, err := json.Marshal(pkg)
	if err != nil {
		return err
	}
	c := lang.ASTCompiler{AST: jsonAST}

	ctx, span := dependency.Inject(context.Background(),
		executetest.NewTestExecuteDependencies(),
		testing.FrameworkConfig{},
	)
	defer span.Finish()
	program, err := c.Compile(ctx, runtime.Default)
	if err != nil {
		return errors.Wrap(err, codes.Invalid, "failed to compile")
	}

	mem := arrowmem.NewCheckedAllocator(arrowmem.DefaultAllocator)
	alloc := &memory.ResourceAllocator{Allocator: mem}
	query, err := program.Start(ctx, alloc)
	if err != nil {
		return errors.Wrap(err, codes.Inherit, "error while executing program")
	}
	defer query.Done()

	var output strings.Builder
	results := flux.NewResultIteratorFromQuery(query)
	for results.More() {
		result := results.Next()
		err := result.Tables().Do(func(tbl flux.Table) error {
			// The data returned here is the result of `testing.diff`, so any result means that
			// a comparison of two tables showed inequality. Capture that inequality as part of the error.
			// XXX: rockstar (08 Dec 2020) - This could use some ergonomic work, as the diff output
			// is not exactly "human readable."
			_, _ = fmt.Fprint(&output, table.Stringify(tbl))
			return nil
		})
		if err != nil {
			return err
		}
	}
	results.Release()

	err = results.Err()
	if err == nil && output.Len() > 0 {
		err = errors.Newf(codes.FailedPrecondition, "Expected test to have no output. Got:\n%s", output.String())
	}

	logger := consoleLogger{}
	mem.AssertSize(&logger, 0)
	if logger.errs > 0 {
		err = errors.New(codes.FailedPrecondition, "Memory leak detected")
	}
	return err
}

func (testExecutor) Close() error { return nil }
