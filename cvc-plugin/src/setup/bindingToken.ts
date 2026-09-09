export class BindingToken {
  private active = true;
  isActive(): boolean { return this.active; }
  cancel(): void { this.active = false; }
}
