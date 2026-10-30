namespace Robust.Shared.Physics.Backend;

internal interface IPhysicsBackend
{
    string Name { get; }

    void Step(float frameTime);

    void Shutdown();
}
