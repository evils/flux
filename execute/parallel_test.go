package execute_test

import (
	"context"
	"fmt"
	"math"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/influxdata/flux"
	"github.com/influxdata/flux/execute"
	"github.com/influxdata/flux/execute/executetest"
	_ "github.com/influxdata/flux/fluxinit/static"
	"github.com/influxdata/flux/interpreter"
	"github.com/influxdata/flux/memory"
	"github.com/influxdata/flux/plan"
	"github.com/influxdata/flux/plan/plantest"
	"github.com/influxdata/flux/runtime"
	"github.com/influxdata/flux/stdlib/universe"
	"go.uber.org/zap/zaptest"
)

func init() {
	// We depend on the registrations that happen in executor_test.go
}

type physicalNodeOption func(*plan.PhysicalPlanNode)

func withOutputAttr(name string, attr plan.PhysicalAttr) physicalNodeOption {
	return func(node *plan.PhysicalPlanNode) {
		node.SetOutputAttr(name, attr)
	}
}

func withRequiredAttr(name string, attr plan.PhysicalAttr) physicalNodeOption {
	return func(node *plan.PhysicalPlanNode) {
		node.SetRequiredAttr(name, attr)
	}
}

func createPhysicalNode(id plan.NodeID, spec plan.PhysicalProcedureSpec, opts ...physicalNodeOption) *plan.PhysicalPlanNode {
	node := plan.CreatePhysicalNode(id, spec)
	for _, opt := range opts {
		opt(node)
	}
	return node
}

func TestParallel_Execute(t *testing.T) {

	testcases := []struct {
		name              string
		spec              *plantest.PlanSpec
		want              map[string][]*executetest.Table
		allocator         *memory.Allocator
		wantErr           error
		wantValidationErr error
	}{
		{
			name: `parallel-from`,
			spec: &plantest.PlanSpec{
				Nodes: []plan.Node{
					createPhysicalNode("from-test",
						executetest.NewFromProcedureSpec(
							[]*executetest.Table{
								{
									KeyCols: []string{"_start", "_stop"},
									ColMeta: []flux.ColMeta{
										{Label: "_start", Type: flux.TTime},
										{Label: "_stop", Type: flux.TTime},
										{Label: "_time", Type: flux.TTime},
										{Label: "_value", Type: flux.TFloat},
										{Label: "_parallel_group", Type: flux.TInt},
									},
									Data: [][]interface{}{
										{execute.Time(0), execute.Time(5), execute.Time(0), 1.0, -1},
										{execute.Time(0), execute.Time(5), execute.Time(1), 2.0, -1},
										{execute.Time(0), execute.Time(5), execute.Time(2), 3.0, -1},
										{execute.Time(0), execute.Time(5), execute.Time(3), 4.0, -1},
										{execute.Time(0), execute.Time(5), execute.Time(4), 5.0, -1},
									},
									ResidesOnPartition: 0,
								},
								{
									KeyCols: []string{"_start", "_stop"},
									ColMeta: []flux.ColMeta{
										{Label: "_start", Type: flux.TTime},
										{Label: "_stop", Type: flux.TTime},
										{Label: "_time", Type: flux.TTime},
										{Label: "_value", Type: flux.TFloat},
										{Label: "_parallel_group", Type: flux.TInt},
									},
									Data: [][]interface{}{
										{execute.Time(5), execute.Time(10), execute.Time(5), 5.0, -1},
										{execute.Time(5), execute.Time(10), execute.Time(6), 6.0, -1},
										{execute.Time(5), execute.Time(10), execute.Time(7), 7.0, -1},
										{execute.Time(5), execute.Time(10), execute.Time(8), 8.0, -1},
										{execute.Time(5), execute.Time(10), execute.Time(9), 9.0, -1},
									},
									ResidesOnPartition: 1,
								},
							}),
						withOutputAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2})),
					createPhysicalNode("merge", &universe.PartitionMergeProcedureSpec{},
						withRequiredAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2}),
						withOutputAttr(plan.ParallelMergeKey, plan.ParallelMergeAttribute{Factor: 2})),
					createPhysicalNode("filter", &universe.FilterProcedureSpec{
						Fn: interpreter.ResolvedFunction{
							Scope: runtime.Prelude(),
							Fn:    executetest.FunctionExpression(t, "(r) => r._value < 7.5"),
						},
					}),
					createPhysicalNode("yield", executetest.NewYieldProcedureSpec("_result")),
				},
				Edges: [][2]int{
					{0, 1},
					{1, 2},
					{2, 3},
				},
			},
			want: map[string][]*executetest.Table{
				"_result": []*executetest.Table{
					{
						KeyCols: []string{"_start", "_stop"},
						ColMeta: []flux.ColMeta{
							{Label: "_start", Type: flux.TTime},
							{Label: "_stop", Type: flux.TTime},
							{Label: "_time", Type: flux.TTime},
							{Label: "_value", Type: flux.TFloat},
							{Label: "_parallel_group", Type: flux.TInt},
						},
						Data: [][]interface{}{
							{execute.Time(0), execute.Time(5), execute.Time(0), 1.0, int64(0)},
							{execute.Time(0), execute.Time(5), execute.Time(1), 2.0, int64(0)},
							{execute.Time(0), execute.Time(5), execute.Time(2), 3.0, int64(0)},
							{execute.Time(0), execute.Time(5), execute.Time(3), 4.0, int64(0)},
							{execute.Time(0), execute.Time(5), execute.Time(4), 5.0, int64(0)},
						},
					},
					{
						KeyCols: []string{"_start", "_stop"},
						ColMeta: []flux.ColMeta{
							{Label: "_start", Type: flux.TTime},
							{Label: "_stop", Type: flux.TTime},
							{Label: "_time", Type: flux.TTime},
							{Label: "_value", Type: flux.TFloat},
							{Label: "_parallel_group", Type: flux.TInt},
						},
						Data: [][]interface{}{
							{execute.Time(5), execute.Time(10), execute.Time(5), 5.0, int64(1)},
							{execute.Time(5), execute.Time(10), execute.Time(6), 6.0, int64(1)},
							{execute.Time(5), execute.Time(10), execute.Time(7), 7.0, int64(1)},
						},
					},
				},
			},
		},
		{
			name: `parallel-from-filter`,
			spec: &plantest.PlanSpec{
				Nodes: []plan.Node{
					createPhysicalNode("from-test",
						executetest.NewFromProcedureSpec(
							[]*executetest.Table{
								{
									KeyCols: []string{"_start", "_stop"},
									ColMeta: []flux.ColMeta{
										{Label: "_start", Type: flux.TTime},
										{Label: "_stop", Type: flux.TTime},
										{Label: "_time", Type: flux.TTime},
										{Label: "_value", Type: flux.TFloat},
										{Label: "_parallel_group", Type: flux.TInt},
									},
									Data: [][]interface{}{
										{execute.Time(0), execute.Time(5), execute.Time(0), 1.0, -1},
										{execute.Time(0), execute.Time(5), execute.Time(1), 2.0, -1},
										{execute.Time(0), execute.Time(5), execute.Time(2), 3.0, -1},
										{execute.Time(0), execute.Time(5), execute.Time(3), 4.0, -1},
										{execute.Time(0), execute.Time(5), execute.Time(4), 5.0, -1},
									},
									ResidesOnPartition: 0,
								},
								{
									KeyCols: []string{"_start", "_stop"},
									ColMeta: []flux.ColMeta{
										{Label: "_start", Type: flux.TTime},
										{Label: "_stop", Type: flux.TTime},
										{Label: "_time", Type: flux.TTime},
										{Label: "_value", Type: flux.TFloat},
										{Label: "_parallel_group", Type: flux.TInt},
									},
									Data: [][]interface{}{
										{execute.Time(5), execute.Time(10), execute.Time(5), 5.0, -1},
										{execute.Time(5), execute.Time(10), execute.Time(6), 6.0, -1},
										{execute.Time(5), execute.Time(10), execute.Time(7), 7.0, -1},
										{execute.Time(5), execute.Time(10), execute.Time(8), 8.0, -1},
										{execute.Time(5), execute.Time(10), execute.Time(9), 9.0, -1},
									},
									ResidesOnPartition: 1,
								},
							}),
						withOutputAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2})),
					createPhysicalNode("filter",
						&universe.FilterProcedureSpec{
							Fn: interpreter.ResolvedFunction{
								Scope: runtime.Prelude(),
								Fn:    executetest.FunctionExpression(t, "(r) => r._value < 7.5"),
							},
						},
						withRequiredAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2}),
						withOutputAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2})),
					createPhysicalNode("merge", &universe.PartitionMergeProcedureSpec{},
						withRequiredAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2}),
						withOutputAttr(plan.ParallelMergeKey, plan.ParallelMergeAttribute{Factor: 2})),
					createPhysicalNode("yield", executetest.NewYieldProcedureSpec("_result")),
				},
				Edges: [][2]int{
					{0, 1},
					{1, 2},
					{2, 3},
				},
			},
			want: map[string][]*executetest.Table{
				"_result": []*executetest.Table{
					{
						KeyCols: []string{"_start", "_stop"},
						ColMeta: []flux.ColMeta{
							{Label: "_start", Type: flux.TTime},
							{Label: "_stop", Type: flux.TTime},
							{Label: "_time", Type: flux.TTime},
							{Label: "_value", Type: flux.TFloat},
							{Label: "_parallel_group", Type: flux.TInt},
						},
						Data: [][]interface{}{
							{execute.Time(0), execute.Time(5), execute.Time(0), 1.0, int64(0)},
							{execute.Time(0), execute.Time(5), execute.Time(1), 2.0, int64(0)},
							{execute.Time(0), execute.Time(5), execute.Time(2), 3.0, int64(0)},
							{execute.Time(0), execute.Time(5), execute.Time(3), 4.0, int64(0)},
							{execute.Time(0), execute.Time(5), execute.Time(4), 5.0, int64(0)},
						},
					},
					{
						KeyCols: []string{"_start", "_stop"},
						ColMeta: []flux.ColMeta{
							{Label: "_start", Type: flux.TTime},
							{Label: "_stop", Type: flux.TTime},
							{Label: "_time", Type: flux.TTime},
							{Label: "_value", Type: flux.TFloat},
							{Label: "_parallel_group", Type: flux.TInt},
						},
						Data: [][]interface{}{
							{execute.Time(5), execute.Time(10), execute.Time(5), 5.0, int64(1)},
							{execute.Time(5), execute.Time(10), execute.Time(6), 6.0, int64(1)},
							{execute.Time(5), execute.Time(10), execute.Time(7), 7.0, int64(1)},
						},
					},
				},
			},
		},
		{
			name: `from-missing-output`,
			spec: &plantest.PlanSpec{
				Nodes: []plan.Node{
					createPhysicalNode("from-test",
						executetest.NewFromProcedureSpec([]*executetest.Table{})),
					createPhysicalNode("filter",
						&universe.FilterProcedureSpec{
							Fn: interpreter.ResolvedFunction{
								Scope: runtime.Prelude(),
								Fn:    executetest.FunctionExpression(t, "(r) => r._value < 7.5"),
							},
						},
						withRequiredAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2}),
						withOutputAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2})),
					createPhysicalNode("merge", &universe.PartitionMergeProcedureSpec{},
						withRequiredAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2}),
						withOutputAttr(plan.ParallelMergeKey, plan.ParallelMergeAttribute{Factor: 2})),
					createPhysicalNode("yield", executetest.NewYieldProcedureSpec("_result")),
				},
				Edges: [][2]int{
					{0, 1},
					{1, 2},
					{2, 3},
				},
			},
			wantValidationErr: fmt.Errorf("invalid physical query plan; attribute \"parallel-run\" " +
				"required by \"filter\" is missing from predecessor \"from-test\""),
		},
		{
			name: `from-missing-required`,
			spec: &plantest.PlanSpec{
				Nodes: []plan.Node{
					createPhysicalNode("from-test",
						executetest.NewFromProcedureSpec([]*executetest.Table{}),
						withOutputAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2})),
					createPhysicalNode("filter",
						&universe.FilterProcedureSpec{
							Fn: interpreter.ResolvedFunction{
								Scope: runtime.Prelude(),
								Fn:    executetest.FunctionExpression(t, "(r) => r._value < 7.5"),
							},
						},
						withOutputAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2})),
					createPhysicalNode("merge", &universe.PartitionMergeProcedureSpec{},
						withRequiredAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2}),
						withOutputAttr(plan.ParallelMergeKey, plan.ParallelMergeAttribute{Factor: 2})),
					createPhysicalNode("yield", executetest.NewYieldProcedureSpec("_result")),
				},
				Edges: [][2]int{
					{0, 1},
					{1, 2},
					{2, 3},
				},
			},
			wantValidationErr: fmt.Errorf("invalid physical query plan; attribute \"parallel-run\" " +
				"on \"from-test\" must be required by all successors, but isn't on \"filter\""),
		},
		{
			name: `from-factor-mismatch`,
			spec: &plantest.PlanSpec{
				Nodes: []plan.Node{
					createPhysicalNode("from-test",
						executetest.NewFromProcedureSpec([]*executetest.Table{}),
						withOutputAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 2})),
					createPhysicalNode("filter",
						&universe.FilterProcedureSpec{
							Fn: interpreter.ResolvedFunction{
								Scope: runtime.Prelude(),
								Fn:    executetest.FunctionExpression(t, "(r) => r._value < 7.5"),
							},
						},
						withRequiredAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 1}),
						withOutputAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 1})),
					createPhysicalNode("merge", &universe.PartitionMergeProcedureSpec{},
						withRequiredAttr(plan.ParallelRunKey, plan.ParallelRunAttribute{Factor: 1}),
						withOutputAttr(plan.ParallelMergeKey, plan.ParallelMergeAttribute{Factor: 1})),
					createPhysicalNode("yield", executetest.NewYieldProcedureSpec("_result")),
				},
				Edges: [][2]int{
					{0, 1},
					{1, 2},
					{2, 3},
				},
			},
			wantValidationErr: fmt.Errorf("invalid physical query plan; attribute \"parallel-run\" " +
				"required by \"filter\" does not match attribute in predecessor \"from-test\""),
		},
	}

	for _, tc := range testcases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {

			tc.spec.Resources = flux.ResourceManagement{
				ConcurrencyQuota: 1,
				MemoryBytesQuota: math.MaxInt64,
			}

			tc.spec.Now = time.Now()

			// Construct physical query plan
			ps := plantest.CreatePlanSpec(tc.spec)

			err := plan.ValidateAttributes(ps)
			if tc.wantValidationErr == nil && err != nil {
				t.Fatal(err)
			}

			if tc.wantValidationErr != nil {
				if err == nil {
					t.Fatalf(`expected an error "%v" but got none`, tc.wantValidationErr)
				}

				if diff := cmp.Diff(tc.wantValidationErr.Error(), err.Error()); diff != "" {
					t.Fatalf("unexpected error: -want/+got: %v", diff)
				}
				return
			}

			exe := execute.NewExecutor(zaptest.NewLogger(t))

			alloc := tc.allocator
			if alloc == nil {
				alloc = executetest.UnlimitedAllocator
			}

			// Execute the query and preserve any error returned
			ctx := executetest.NewTestExecuteDependencies().Inject(context.Background())
			results, _, err := exe.Execute(ctx, ps, alloc)
			var got map[string][]*executetest.Table
			if err == nil {
				got = make(map[string][]*executetest.Table, len(results))
				for name, r := range results {
					if err = r.Tables().Do(func(tbl flux.Table) error {
						cb, err := executetest.ConvertTable(tbl)
						if err != nil {
							return err
						}
						got[name] = append(got[name], cb)
						return nil
					}); err != nil {
						break
					}
				}
			}

			if tc.wantErr == nil && err != nil {
				t.Fatal(err)
			}

			if tc.wantErr != nil {
				if err == nil {
					t.Fatalf(`expected an error "%v" but got none`, tc.wantErr)
				}

				if diff := cmp.Diff(tc.wantErr, err); diff != "" {
					t.Fatalf("unexpected error: -want/+got: %v", diff)
				}
				return
			}

			for _, g := range got {
				executetest.NormalizeTables(g)
			}
			for _, w := range tc.want {
				executetest.NormalizeTables(w)
			}

			if !cmp.Equal(got, tc.want) {
				t.Error("unexpected results -want/+got", cmp.Diff(tc.want, got))
			}
		})
	}
}