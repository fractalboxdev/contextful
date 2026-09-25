# Diagrams

How the spec's diagrams stay readable: a flowchart draws things as nodes and what flows
between them as labelled edges, a sequence diagram shows one scenario, and a state
diagram shows a lifecycle the system can run. `corpus.diagram` states the rules and
`contextful-spec lint --check diagram` enforces them over every `mermaid` fence.

## Practices

| Practice | Source | Informs | What the corpus takes from it |
| --- | --- | --- | --- |
| C4 review checklist and notation: every element and relationship labelled, every line one-directional with a label matching its direction, one notation per diagram | <https://c4model.com/diagrams/checklist> · <https://c4model.com/diagrams/notation> | `corpus.diagram` (`edge`, `unique-label`, `shape`, `layout`) | An edge names what flows in at most 5 words and points one way; one shape means one kind across a diagram; one declared reading direction. |
| ISO 5807 flowchart symbols: a decision symbol with one entry and a labelled exit per outcome | <https://www.iso.org/standard/11955.html> | `corpus.diagram` (`decision`, `branch`) | A branch is a `{ }` decision with an incoming edge and two or more distinctly labelled exits; a box never branches through its edge labels. |
| UML 2.5.1 state machines (§14) and interactions (§17) | <https://www.omg.org/spec/UML/2.5.1> | `corpus.diagram` (`state`, `sequence`) | One initial pseudostate, every state reachable, no dead end; lifelines declared and used, combined fragments nested shallowly. |
| Mermaid flowchart, sequence and state grammar | <https://mermaid.js.org/syntax/flowchart.html> · <https://mermaid.js.org/syntax/sequenceDiagram.html> · <https://mermaid.js.org/syntax/stateDiagram.html> | `corpus.diagram` | The grammar the checker parses: node shapes, `&` fan-out, pipe and inline edge labels, subgraphs, fragments and composite states. |
| arc42 building-block and runtime views | <https://docs.arc42.org> | `corpus.diagram` (`connected`, `layout`, `boundary`) | Containers hold components and take edges on their behalf; a level of nesting past container and component is a second diagram. |

## Notation and cognition

### The Physics of Notations

Moody, D. "The 'Physics' of Notations: Toward a Scientific Basis for Constructing
Visual Notations in Software Engineering." *IEEE Transactions on Software Engineering*
35(6), 2009. <https://doi.org/10.1109/TSE.2009.67>

- **Priority:** must-read
- **Informs:** `corpus.diagram`
- **Question:** What makes a notation cognitively effective? Semiotic clarity (one
  symbol, one meaning) grounds `shape` and `unique-label`; complexity management grounds
  the node, edge and nesting bounds in `layout`.

### The Magical Number Seven, Plus or Minus Two

Miller, G. A. "The Magical Number Seven, Plus or Minus Two: Some Limits on Our Capacity
for Processing Information." *Psychological Review* 63(2), 1956.
<https://doi.org/10.1037/h0043158>

- **Priority:** optional
- **Informs:** `corpus.diagram`
- **Question:** How many chunks does a reader hold at once? The origin of the small
  caps on node words and sequence participants.

### The magical number 4 in short-term memory

Cowan, N. "The magical number 4 in short-term memory: A reconsideration of mental
storage capacity." *Behavioral and Brain Sciences* 24(1), 2001.
<https://doi.org/10.1017/S0140525X01003922>

- **Priority:** should-read
- **Informs:** `corpus.diagram`
- **Question:** Is seven the right working-memory figure? Cowan's estimate of about four
  chunks sets the 5-word caps on node and edge labels.

## UML style

### UML Distilled

Fowler, M. *UML Distilled: A Brief Guide to the Standard Object Modeling Language*,
3rd ed., chapter 4. Addison-Wesley, 2003. <https://martinfowler.com/books/uml.html>

- **Priority:** should-read
- **Informs:** `corpus.diagram`
- **Question:** What is a sequence diagram for? One scenario's interactions; conditional
  logic past a fragment or two belongs in an activity diagram, which `sequence` enforces
  through its participant, message and fragment-depth bounds.

### The Elements of UML 2.0 Style

Ambler, S. W. *The Elements of UML 2.0 Style*. Cambridge University Press, 2005.
ISBN 978-0-521-61678-2.

- **Priority:** optional
- **Informs:** `corpus.diagram`
- **Question:** Which style guidelines keep sequence and state diagrams legible? Short
  message labels, guards on every decision exit, and no black-hole or miracle states
  back `message`, `decision` and `state`.
