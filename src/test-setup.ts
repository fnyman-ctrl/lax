import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// happy-dom has no layout, so scrolling into view is a no-op.
Element.prototype.scrollIntoView = () => {};

afterEach(cleanup);
